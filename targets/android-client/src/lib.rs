//! Android client for SonicSpan: receiver and transmitter.
//!
//! The receiver thread gets UDP packets from `desktop-node transmit`, decodes
//! PCM or Opus, runs them through the jitter buffer and drift compensator,
//! and plays the result through an `AudioTrack`. The transmitter thread
//! captures the microphone through `AudioRecord` and sends PCM or Opus
//! packets to `desktop-node receive`.

mod audio;
mod receiver;
mod transmitter;

use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result};
use jni::objects::{GlobalRef, JObject, JString, JValue};
use jni::sys::{jfloat, jint};
use jni::{JNIEnv, JavaVM};
use receiver::{Receiver, ReceiverConfig, ReceiverStats};
use span_core::traits::NetworkTransport;
use span_transport::protocol::{PcmPacket, HEADER_SIZE};
use span_transport::udp::UdpTransport;
use tokio::sync::Notify;

use crate::audio::AndroidAudioPlayer;
use crate::audio::AndroidMicCapture;
use crate::transmitter::{Transmitter, TransmitterConfig};

struct Shared {
    vm: JavaVM,
    stop: AtomicBool,
    notify: Notify,
    /// Global ref to the current `AudioTrack`, for UI-thread stop/volume.
    track: Mutex<Option<GlobalRef>>,
    listener: GlobalRef,
}

struct AndroidReceiver {
    shared: Arc<Shared>,
    thread: Option<std::thread::JoinHandle<()>>,
}

static RECEIVER: Mutex<Option<AndroidReceiver>> = Mutex::new(None);

struct TxShared {
    vm: JavaVM,
    stop: AtomicBool,
    notify: Notify,
    /// Global ref to the current `AudioRecord`, for UI-thread stop.
    record: Mutex<Option<GlobalRef>>,
    listener: GlobalRef,
}

struct AndroidTransmitter {
    shared: Arc<TxShared>,
    thread: Option<std::thread::JoinHandle<()>>,
}

static TRANSMITTER: Mutex<Option<AndroidTransmitter>> = Mutex::new(None);

fn notify_status(env: &mut JNIEnv, listener: &GlobalRef, message: &str) {
    if let Ok(js) = env.new_string(message) {
        let _ = env.call_method(
            listener.as_obj(),
            "onStatus",
            "(Ljava/lang/String;)V",
            &[JValue::Object(&js)],
        );
    }
}

fn notify_stats(env: &mut JNIEnv, listener: &GlobalRef, stats: &ReceiverStats) {
    let _ = env.call_method(
        listener.as_obj(),
        "onStats",
        "(JJJ)V",
        &[
            JValue::Long(stats.packets_received as i64),
            JValue::Long(stats.packets_lost as i64),
            JValue::Long(stats.underruns as i64),
        ],
    );
}

fn notify_tx_stats(env: &mut JNIEnv, listener: &GlobalRef, packets: u64, bytes: u64) {
    let _ = env.call_method(
        listener.as_obj(),
        "onTxStats",
        "(JJ)V",
        &[JValue::Long(packets as i64), JValue::Long(bytes as i64)],
    );
}

/// Start receiving from `desktop-node transmit` at `host:port` (UDP).
#[no_mangle]
pub extern "system" fn Java_com_sonicspan_MainActivity_nativeStart(
    mut env: JNIEnv,
    this: JObject,
    host: JString,
    port: jint,
) {
    let host: String = match env.get_string(&host) {
        Ok(value) => value.into(),
        Err(e) => {
            let _ = env.exception_clear();
            eprintln!("[android-client] failed to read host string: {e}");
            return;
        }
    };

    let mut slot = match RECEIVER.lock() {
        Ok(slot) => slot,
        Err(_) => return,
    };
    if let Some(existing) = slot.as_ref() {
        notify_status(
            &mut env,
            &existing.shared.listener,
            "already running; press Stop first",
        );
        return;
    }

    let vm = match env.get_java_vm() {
        Ok(vm) => vm,
        Err(e) => {
            let _ = env.exception_clear();
            eprintln!("[android-client] failed to get JavaVM: {e}");
            return;
        }
    };
    let listener = match env.new_global_ref(&this) {
        Ok(listener) => listener,
        Err(e) => {
            let _ = env.exception_clear();
            eprintln!("[android-client] failed to keep listener reference: {e}");
            return;
        }
    };
    let shared = Arc::new(Shared {
        vm,
        stop: AtomicBool::new(false),
        notify: Notify::new(),
        track: Mutex::new(None),
        listener,
    });

    let thread_shared = Arc::clone(&shared);
    let thread = std::thread::spawn(move || {
        let mut env = match thread_shared.vm.attach_current_thread() {
            Ok(env) => env,
            Err(e) => {
                eprintln!("[android-client] failed to attach thread: {e}");
                return;
            }
        };
        notify_status(&mut env, &thread_shared.listener, "connecting");

        let addr: SocketAddr = match format!("{host}:{port}").parse() {
            Ok(addr) => addr,
            Err(e) => {
                notify_status(
                    &mut env,
                    &thread_shared.listener,
                    &format!("invalid address: {e}"),
                );
                // SAFETY: this thread was attached by `attach_current_thread`
                // above and is about to exit.
                unsafe {
                    thread_shared.vm.detach_current_thread();
                }
                return;
            }
        };
        let runtime = match tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        {
            Ok(runtime) => runtime,
            Err(e) => {
                notify_status(
                    &mut env,
                    &thread_shared.listener,
                    &format!("failed to start runtime: {e}"),
                );
                // SAFETY: this thread was attached by `attach_current_thread`
                // above and is about to exit.
                unsafe {
                    thread_shared.vm.detach_current_thread();
                }
                return;
            }
        };

        let result = runtime.block_on(async {
            let mut transport = match UdpTransport::bind(addr).await {
                Ok(transport) => transport,
                Err(e) => {
                    notify_status(
                        &mut env,
                        &thread_shared.listener,
                        &format!("failed to bind: {e}"),
                    );
                    return Ok(());
                }
            };
            run_loop(&mut env, &thread_shared, &mut transport).await
        });
        if let Err(e) = result {
            notify_status(&mut env, &thread_shared.listener, &format!("error: {e:#}"));
        }
        // SAFETY: this thread was attached by `attach_current_thread` above
        // and is about to exit.
        unsafe {
            thread_shared.vm.detach_current_thread();
        }
    });

    *slot = Some(AndroidReceiver {
        shared,
        thread: Some(thread),
    });
}

/// Stop the receiver and release audio resources.
#[no_mangle]
pub extern "system" fn Java_com_sonicspan_MainActivity_nativeStop(mut env: JNIEnv, _this: JObject) {
    let mut slot = match RECEIVER.lock() {
        Ok(slot) => slot,
        Err(_) => return,
    };
    if let Some(mut receiver) = slot.take() {
        receiver.shared.stop.store(true, Ordering::SeqCst);
        receiver.shared.notify.notify_one();
        // Unblock a blocking AudioTrack.write from the UI thread.
        if let Some(track) = receiver.shared.track.lock().unwrap().as_ref() {
            let _ = env.call_method(track.as_obj(), "stop", "()V", &[]);
        }
        if let Some(thread) = receiver.thread.take() {
            let _ = thread.join();
        }
    }

    // Stop an active transmitter as well.
    if let Ok(mut slot) = TRANSMITTER.lock() {
        if let Some(mut transmitter) = slot.take() {
            transmitter.shared.stop.store(true, Ordering::SeqCst);
            transmitter.shared.notify.notify_one();
            // Unblock a blocking AudioRecord.read from the UI thread.
            if let Some(record) = transmitter.shared.record.lock().unwrap().as_ref() {
                let _ = env.call_method(record.as_obj(), "stop", "()V", &[]);
            }
            if let Some(thread) = transmitter.thread.take() {
                let _ = thread.join();
            }
        }
    }
}

/// Adjust playback volume (0.0–1.0).
#[no_mangle]
pub extern "system" fn Java_com_sonicspan_MainActivity_nativeSetVolume(
    mut env: JNIEnv,
    _this: JObject,
    volume: jfloat,
) {
    if let Ok(slot) = RECEIVER.lock() {
        if let Some(receiver) = slot.as_ref() {
            if let Some(track) = receiver.shared.track.lock().unwrap().as_ref() {
                let _ = env.call_method(
                    track.as_obj(),
                    "setVolume",
                    "(F)I",
                    &[JValue::Float(volume)],
                );
            }
        }
    }
}

/// Start transmitting microphone audio to `desktop-node receive` at
/// `host:port` (UDP, mono 48 kHz, Opus).
#[no_mangle]
pub extern "system" fn Java_com_sonicspan_MainActivity_nativeStartTransmit(
    mut env: JNIEnv,
    this: JObject,
    host: JString,
    port: jint,
) {
    let host: String = match env.get_string(&host) {
        Ok(value) => value.into(),
        Err(e) => {
            let _ = env.exception_clear();
            eprintln!("[android-client] failed to read host string: {e}");
            return;
        }
    };

    let mut slot = match TRANSMITTER.lock() {
        Ok(slot) => slot,
        Err(_) => return,
    };
    if slot.is_some() {
        if let Some(existing) = slot.as_ref() {
            notify_status(
                &mut env,
                &existing.shared.listener,
                "already transmitting; press Stop first",
            );
        }
        return;
    }

    let vm = match env.get_java_vm() {
        Ok(vm) => vm,
        Err(e) => {
            let _ = env.exception_clear();
            eprintln!("[android-client] failed to get JavaVM: {e}");
            return;
        }
    };
    let listener = match env.new_global_ref(&this) {
        Ok(listener) => listener,
        Err(e) => {
            let _ = env.exception_clear();
            eprintln!("[android-client] failed to keep listener reference: {e}");
            return;
        }
    };
    let shared = Arc::new(TxShared {
        vm,
        stop: AtomicBool::new(false),
        notify: Notify::new(),
        record: Mutex::new(None),
        listener,
    });

    let thread_shared = Arc::clone(&shared);
    let thread = std::thread::spawn(move || {
        let mut env = match thread_shared.vm.attach_current_thread() {
            Ok(env) => env,
            Err(e) => {
                eprintln!("[android-client] failed to attach thread: {e}");
                return;
            }
        };

        let addr: SocketAddr = match format!("{host}:{port}").parse() {
            Ok(addr) => addr,
            Err(e) => {
                notify_status(
                    &mut env,
                    &thread_shared.listener,
                    &format!("invalid address: {e}"),
                );
                // SAFETY: this thread was attached by `attach_current_thread`
                // above and is about to exit.
                unsafe {
                    thread_shared.vm.detach_current_thread();
                }
                return;
            }
        };
        let socket = match std::net::UdpSocket::bind("0.0.0.0:0") {
            Ok(socket) => socket,
            Err(e) => {
                notify_status(
                    &mut env,
                    &thread_shared.listener,
                    &format!("failed to open UDP socket: {e}"),
                );
                // SAFETY: this thread was attached by `attach_current_thread`
                // above and is about to exit.
                unsafe {
                    thread_shared.vm.detach_current_thread();
                }
                return;
            }
        };
        if let Err(e) = socket.connect(addr) {
            notify_status(
                &mut env,
                &thread_shared.listener,
                &format!("failed to connect to {addr}: {e}"),
            );
            // SAFETY: this thread was attached by `attach_current_thread`
            // above and is about to exit.
            unsafe {
                thread_shared.vm.detach_current_thread();
            }
            return;
        }

        let mic = match AndroidMicCapture::create(&mut env, 48_000) {
            Ok(mic) => mic,
            Err(e) => {
                notify_status(
                    &mut env,
                    &thread_shared.listener,
                    &format!("microphone unavailable: {e:#}"),
                );
                // SAFETY: this thread was attached by `attach_current_thread`
                // above and is about to exit.
                unsafe {
                    thread_shared.vm.detach_current_thread();
                }
                return;
            }
        };
        *thread_shared.record.lock().unwrap() = Some(mic.global_ref());

        let mut transmitter = match Transmitter::new(TransmitterConfig::default()) {
            Ok(transmitter) => transmitter,
            Err(e) => {
                notify_status(
                    &mut env,
                    &thread_shared.listener,
                    &format!("failed to create transmitter: {e}"),
                );
                // SAFETY: this thread was attached by `attach_current_thread`
                // above and is about to exit.
                unsafe {
                    thread_shared.vm.detach_current_thread();
                }
                return;
            }
        };
        notify_status(
            &mut env,
            &thread_shared.listener,
            &format!("transmitting to {addr} (48 kHz mono, Opus)"),
        );

        let mut pcm = vec![0i16; 8192];
        let mut floats = vec![0.0f32; 8192];
        let mut sent_packets = 0u64;
        let mut sent_bytes = 0u64;
        loop {
            if thread_shared.stop.load(Ordering::SeqCst) {
                break;
            }
            let n = match mic.read_i16(&mut env, &mut pcm) {
                Ok(n) => n,
                Err(e) => {
                    notify_status(
                        &mut env,
                        &thread_shared.listener,
                        &format!("microphone read failed: {e:#}"),
                    );
                    break;
                }
            };
            if n == 0 {
                if thread_shared.stop.load(Ordering::SeqCst) {
                    break;
                }
                std::thread::sleep(Duration::from_millis(5));
                continue;
            }
            for (i, sample) in floats.iter_mut().take(n).enumerate() {
                *sample = pcm[i] as f32 / 32_768.0;
            }
            match transmitter.process_pcm(&floats[..n]) {
                Ok(packets) => {
                    for packet in packets {
                        let mut bytes = Vec::with_capacity(HEADER_SIZE + packet.payload.len());
                        packet.encode(&mut bytes);
                        if socket.send(&bytes).is_ok() {
                            sent_packets += 1;
                            sent_bytes += bytes.len() as u64;
                        }
                    }
                }
                Err(e) => notify_status(
                    &mut env,
                    &thread_shared.listener,
                    &format!("encode failed: {e}"),
                ),
            }
            if sent_packets > 0 && sent_packets.is_multiple_of(100) {
                notify_tx_stats(&mut env, &thread_shared.listener, sent_packets, sent_bytes);
            }
        }

        let _ = mic.stop(&mut env);
        let _ = mic.release(&mut env);
        *thread_shared.record.lock().unwrap() = None;
        notify_status(&mut env, &thread_shared.listener, "transmit stopped");
        // SAFETY: this thread was attached by `attach_current_thread` above
        // and is about to exit.
        unsafe {
            thread_shared.vm.detach_current_thread();
        }
    });

    *slot = Some(AndroidTransmitter {
        shared,
        thread: Some(thread),
    });
}

async fn run_loop(
    env: &mut JNIEnv<'_>,
    shared: &Shared,
    transport: &mut UdpTransport,
) -> Result<()> {
    let mut receiver = Receiver::new(ReceiverConfig::default());
    let mut player: Option<AndroidAudioPlayer> = None;
    let mut player_cfg: Option<(u32, u16)> = None;
    let mut packets = 0u64;

    loop {
        tokio::select! {
            _ = shared.notify.notified() => {
                if shared.stop.load(Ordering::SeqCst) {
                    break;
                }
            }
            data = transport.receive_packet() => {
                let data = data.context("receive failed")?;
                let packet = PcmPacket::decode(&data).context("invalid packet")?;

                let cfg = (packet.sample_rate, packet.channels);
                if player_cfg != Some(cfg) {
                    if let Some(old) = player.take() {
                        let _ = old.release(env);
                    }
                    let new_player =
                        AndroidAudioPlayer::create(env, packet.sample_rate, packet.channels)?;
                    *shared.track.lock().unwrap() = Some(new_player.global_ref());
                    player = Some(new_player);
                    player_cfg = Some(cfg);
                    notify_status(
                        env,
                        &shared.listener,
                        &format!("playing {} Hz / {} ch", cfg.0, cfg.1),
                    );
                }

                let out = receiver.process_packet(&packet)?;
                if !out.is_empty() {
                    player.as_ref().unwrap().write_f32(env, &out)?;
                }
                packets += 1;
                if packets.is_multiple_of(200) {
                    notify_stats(env, &shared.listener, &receiver.stats());
                }
            }
        }
    }

    if let Some(player) = player.take() {
        let _ = player.stop(env);
        let _ = player.release(env);
    }
    *shared.track.lock().unwrap() = None;
    Ok(())
}
