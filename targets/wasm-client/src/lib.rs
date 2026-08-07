//! WASM bindings for the SonicSpan browser receiver.
//!
//! Connect to a desktop node's WebSocket stream, decode the PCM datagrams
//! defined in `span-transport::protocol`, and play them through the Web Audio
//! API via an `AudioWorklet` processor (see `web/worklet.js`).

use std::cell::RefCell;

use js_sys::{ArrayBuffer, Reflect, Uint8Array};
use span_transport::protocol::PcmPacket;
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;
use wasm_bindgen_futures::spawn_local;
use wasm_bindgen_futures::JsFuture;
use web_sys::{
    AudioContext, AudioContextOptions, AudioNode, AudioWorkletNode, AudioWorkletNodeOptions,
    BinaryType, CloseEvent, Event, GainNode, MessageEvent, WebSocket,
};

thread_local! {
    static PLAYER: RefCell<Option<Player>> = const { RefCell::new(None) };
    static STATUS_CALLBACK: RefCell<Option<js_sys::Function>> = const { RefCell::new(None) };
    static STATS_CALLBACK: RefCell<Option<js_sys::Function>> = const { RefCell::new(None) };
}

/// The active receiver session (at most one at a time).
struct Player {
    ws: WebSocket,
    context: Option<AudioContext>,
    worklet: Option<AudioWorkletNode>,
    gain: Option<GainNode>,
    worklet_url: String,
    /// Sample rate / channels of the currently configured audio pipeline.
    sample_rate: u32,
    channels: u16,
    /// Samples received while the audio pipeline is being (re)built.
    pending: Vec<f32>,
    packets: u64,
    bytes: u64,
    _on_open: Closure<dyn FnMut(Event)>,
    _on_message: Closure<dyn FnMut(MessageEvent)>,
    _on_close: Closure<dyn FnMut(CloseEvent)>,
    _on_error: Closure<dyn FnMut(Event)>,
}

/// Connect to a desktop node WebSocket stream and start playing audio.
///
/// `url` is the WebSocket endpoint (e.g. `ws://192.168.1.10:8080/ws`) and
/// `worklet_url` is the AudioWorklet module URL relative to the page
/// (e.g. `./worklet.js`).
#[wasm_bindgen]
pub fn connect(url: &str, worklet_url: &str) -> Result<(), JsValue> {
    let occupied = PLAYER.with(|player| player.borrow().is_some());
    if occupied {
        return Err(JsValue::from_str(
            "SonicSpan is already connected; call stop() first",
        ));
    }

    let ws = WebSocket::new(url)?;
    ws.set_binary_type(BinaryType::Arraybuffer);

    let on_open =
        Closure::wrap(Box::new(|_: Event| emit_status("connected")) as Box<dyn FnMut(Event)>);
    let on_message = Closure::wrap(
        Box::new(|event: MessageEvent| handle_message(event)) as Box<dyn FnMut(MessageEvent)>
    );
    let on_close = Closure::wrap(Box::new(|_: CloseEvent| {
        let was_connected = PLAYER.with(|player| player.borrow().is_some());
        if was_connected {
            emit_status("disconnected");
        }
    }) as Box<dyn FnMut(CloseEvent)>);
    let on_error = Closure::wrap(
        Box::new(|_: Event| emit_status("connection error")) as Box<dyn FnMut(Event)>
    );

    ws.set_onopen(Some(on_open.as_ref().unchecked_ref()));
    ws.set_onmessage(Some(on_message.as_ref().unchecked_ref()));
    ws.set_onclose(Some(on_close.as_ref().unchecked_ref()));
    ws.set_onerror(Some(on_error.as_ref().unchecked_ref()));

    // Create the audio context eagerly so a user gesture (the Connect click)
    // unlocks audio playback on mobile browsers. The pipeline is rebuilt with
    // the sender's real sample rate when the first packet arrives.
    let context = AudioContext::new()?;
    let gain = context.create_gain()?;
    gain.gain().set_value(1.0);
    gain.connect_with_audio_node(context.destination().unchecked_ref::<AudioNode>())?;
    let _ = context.resume();

    PLAYER.with(|player| {
        *player.borrow_mut() = Some(Player {
            ws,
            context: Some(context),
            gain: Some(gain),
            worklet: None,
            worklet_url: worklet_url.to_string(),
            sample_rate: 0,
            channels: 0,
            pending: Vec::new(),
            packets: 0,
            bytes: 0,
            _on_open: on_open,
            _on_message: on_message,
            _on_close: on_close,
            _on_error: on_error,
        });
    });
    emit_status("connecting");
    Ok(())
}

/// Stop streaming, close the WebSocket and the audio context.
#[wasm_bindgen]
pub fn stop() {
    let player = PLAYER.with(|player| player.borrow_mut().take());
    if let Some(player) = player {
        let _ = player.ws.close();
        if let Some(context) = player.context {
            let _ = context.close();
        }
        emit_status("stopped");
    }
}

/// Set the playback volume (0.0–1.0).
#[wasm_bindgen]
pub fn set_volume(volume: f32) {
    PLAYER.with(|player| {
        if let Some(player) = player.borrow().as_ref() {
            if let Some(gain) = player.gain.as_ref() {
                gain.gain().set_value(volume.clamp(0.0, 1.0));
            }
        }
    });
}

/// Register a callback receiving human-readable status strings.
#[wasm_bindgen]
pub fn set_status_callback(callback: Option<js_sys::Function>) {
    STATUS_CALLBACK.with(|cb| *cb.borrow_mut() = callback);
}

/// Register a callback receiving `(packets, bytes)` transfer statistics.
#[wasm_bindgen]
pub fn set_stats_callback(callback: Option<js_sys::Function>) {
    STATS_CALLBACK.with(|cb| *cb.borrow_mut() = callback);
}

fn handle_message(event: MessageEvent) {
    let data = match event.data().dyn_into::<ArrayBuffer>() {
        Ok(buffer) => buffer,
        Err(_) => return,
    };
    let bytes = Uint8Array::new(&data).to_vec();
    let packet = match PcmPacket::decode(&bytes) {
        Ok(packet) => packet,
        Err(_) => {
            emit_status("received an invalid packet");
            return;
        }
    };

    let (needs_rebuild, worklet_url, sample_rate, channels) = PLAYER.with(|player| {
        let mut player = player.borrow_mut();
        let player = match player.as_mut() {
            Some(player) => player,
            None => return (false, String::new(), 0, 0),
        };
        player.packets += 1;
        player.bytes += bytes.len() as u64;
        if player.packets.is_multiple_of(100) {
            emit_stats(player.packets, player.bytes);
        }

        let needs_rebuild = player.worklet.is_none()
            || player.sample_rate != packet.sample_rate
            || player.channels != packet.channels;
        if needs_rebuild {
            // Buffer roughly two seconds so the rebuild doesn't drop the start
            // of the stream.
            let cap = packet.sample_rate as usize * packet.channels as usize * 2;
            if player.pending.len() < cap {
                player.pending.extend_from_slice(&packet.samples);
            }
            (
                true,
                player.worklet_url.clone(),
                packet.sample_rate,
                packet.channels,
            )
        } else {
            (false, String::new(), 0, 0)
        }
    });

    if needs_rebuild {
        spawn_local(async move {
            rebuild_player(worklet_url, sample_rate, channels).await;
        });
        return;
    }

    PLAYER.with(|player| {
        if let Some(player) = player.borrow().as_ref() {
            if let Some(node) = player.worklet.as_ref() {
                post_samples(node, &packet.samples, packet.channels);
            }
        }
    });
}

/// (Re)build the audio pipeline for `sample_rate`/`channels`: create the
/// AudioContext, load the worklet, wire it to the destination, and flush any
/// samples buffered during the rebuild.
async fn rebuild_player(worklet_url: String, sample_rate: u32, channels: u16) {
    let options = AudioContextOptions::new();
    options.set_sample_rate(sample_rate as f32);
    let context = match AudioContext::new_with_context_options(&options) {
        Ok(context) => context,
        Err(error) => {
            emit_status(&format!("failed to create audio context: {error:?}"));
            return;
        }
    };
    let _ = context.resume();

    let worklet = match context.audio_worklet() {
        Ok(worklet) => worklet,
        Err(error) => {
            emit_status(&format!("failed to access audio worklet: {error:?}"));
            return;
        }
    };
    let add_module = match worklet.add_module(&worklet_url) {
        Ok(promise) => JsFuture::from(promise),
        Err(error) => {
            emit_status(&format!("failed to load audio worklet: {error:?}"));
            return;
        }
    };
    if let Err(error) = add_module.await {
        emit_status(&format!("failed to load audio worklet: {error:?}"));
        return;
    }

    let gain = match context.create_gain() {
        Ok(gain) => gain,
        Err(error) => {
            emit_status(&format!("failed to create gain node: {error:?}"));
            return;
        }
    };
    let node_options = AudioWorkletNodeOptions::new();
    node_options.set_number_of_outputs(1);
    node_options.set_output_channel_count(&JsValue::from(vec![channels as u32]));
    let node =
        match AudioWorkletNode::new_with_options(&context, "sonicspan-processor", &node_options) {
            Ok(node) => node,
            Err(error) => {
                emit_status(&format!("failed to create worklet node: {error:?}"));
                return;
            }
        };
    if node
        .connect_with_audio_node(gain.unchecked_ref::<AudioNode>())
        .is_err()
        || gain
            .connect_with_audio_node(context.destination().unchecked_ref::<AudioNode>())
            .is_err()
    {
        emit_status("failed to connect audio graph");
        return;
    }

    let pending = PLAYER.with(|player| {
        let mut player = player.borrow_mut();
        let player = match player.as_mut() {
            Some(player) => player,
            None => return Vec::new(),
        };
        if let Some(old) = player.worklet.take() {
            let _ = old.disconnect();
        }
        if let Some(old) = player.context.take() {
            let _ = old.close();
        }
        player.context = Some(context);
        player.gain = Some(gain);
        player.worklet = Some(node);
        player.sample_rate = sample_rate;
        player.channels = channels;
        std::mem::take(&mut player.pending)
    });

    if !pending.is_empty() {
        PLAYER.with(|player| {
            if let Some(player) = player.borrow().as_ref() {
                if let Some(node) = player.worklet.as_ref() {
                    post_samples(node, &pending, channels);
                }
            }
        });
    }
    emit_status("playing");
}

/// Post an interleaved sample block to the worklet processor.
fn post_samples(node: &AudioWorkletNode, samples: &[f32], channels: u16) {
    let message = js_sys::Object::new();
    let _ = Reflect::set(
        &message,
        &JsValue::from_str("channels"),
        &JsValue::from(channels as u32),
    );
    let _ = Reflect::set(
        &message,
        &JsValue::from_str("samples"),
        &js_sys::Float32Array::from(samples),
    );
    if let Ok(port) = node.port() {
        let _ = port.post_message(&message);
    }
}

fn emit_status(message: &str) {
    STATUS_CALLBACK.with(|callback| {
        if let Some(callback) = callback.borrow().as_ref() {
            let _ = callback.call1(&JsValue::NULL, &JsValue::from_str(message));
        }
    });
}

fn emit_stats(packets: u64, bytes: u64) {
    STATS_CALLBACK.with(|callback| {
        if let Some(callback) = callback.borrow().as_ref() {
            let _ = callback.call2(
                &JsValue::NULL,
                &JsValue::from_f64(packets as f64),
                &JsValue::from_f64(bytes as f64),
            );
        }
    });
}
