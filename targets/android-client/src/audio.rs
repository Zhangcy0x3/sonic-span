//! Android audio output through `AudioTrack` via JNI.

use anyhow::{anyhow, Result};
use jni::objects::{GlobalRef, JByteArray, JValue};
use jni::sys::{jbyte, jint};
use jni::JNIEnv;

// AudioTrack constants (android.media.AudioManager / AudioFormat / AudioTrack).
const STREAM_MUSIC: jint = 3;
const CHANNEL_OUT_MONO: jint = 4;
const CHANNEL_OUT_STEREO: jint = 12;
const ENCODING_PCM_16BIT: jint = 2;
const MODE_STREAM: jint = 1;

// AudioRecord constants (android.media.MediaRecorder / AudioFormat).
const AUDIO_SOURCE_MIC: jint = 1;
const CHANNEL_IN_MONO: jint = 16;

/// A streaming `AudioTrack` that accepts interleaved `f32` frames.
pub struct AndroidAudioPlayer {
    track: GlobalRef,
}

/// A microphone capture session through `AudioRecord` (PCM 16-bit, mono).
pub struct AndroidMicCapture {
    record: GlobalRef,
}

impl AndroidMicCapture {
    /// Create and start an `AudioRecord` capturing mono 16-bit PCM.
    pub fn create(env: &mut JNIEnv, sample_rate: u32) -> Result<Self> {
        let class = env.find_class("android/media/AudioRecord")?;
        let min_buffer = env
            .call_static_method(
                &class,
                "getMinBufferSize",
                "(III)I",
                &[
                    JValue::Int(sample_rate as jint),
                    JValue::Int(CHANNEL_IN_MONO),
                    JValue::Int(ENCODING_PCM_16BIT),
                ],
            )?
            .i()?;
        if min_buffer <= 0 {
            return Err(anyhow!(
                "AudioRecord.getMinBufferSize returned {min_buffer}"
            ));
        }
        let buffer_size = (min_buffer * 2).max(4096);
        let record = env.new_object(
            &class,
            "(IIIII)V",
            &[
                JValue::Int(AUDIO_SOURCE_MIC),
                JValue::Int(sample_rate as jint),
                JValue::Int(CHANNEL_IN_MONO),
                JValue::Int(ENCODING_PCM_16BIT),
                JValue::Int(buffer_size),
            ],
        )?;
        let record = env.new_global_ref(&record)?;
        env.call_method(record.as_obj(), "startRecording", "()V", &[])?;
        Ok(Self { record })
    }

    /// Blocking read of mono 16-bit samples. Returns the number of samples
    /// written (0 when the stream is stopped).
    pub fn read_i16(&self, env: &mut JNIEnv, out: &mut [i16]) -> Result<usize> {
        if out.is_empty() {
            return Ok(0);
        }
        let bytes_len = out.len() * 2;
        let array: JByteArray = env.new_byte_array(bytes_len as jint)?;
        let read = env
            .call_method(
                self.record.as_obj(),
                "read",
                "([BII)I",
                &[
                    JValue::Object(array.as_ref()),
                    JValue::Int(0),
                    JValue::Int(bytes_len as jint),
                ],
            )?
            .i()?;
        if read <= 0 {
            return Ok(0);
        }
        let read = read as usize;
        let mut bytes = vec![0i8; read];
        env.get_byte_array_region(&array, 0, &mut bytes)?;
        let samples = read / 2;
        for (i, sample) in out.iter_mut().take(samples).enumerate() {
            *sample = i16::from_le_bytes([bytes[i * 2] as u8, bytes[i * 2 + 1] as u8]);
        }
        Ok(samples)
    }

    /// Stop capturing (also unblocks a pending blocking `read`).
    pub fn stop(&self, env: &mut JNIEnv) -> Result<()> {
        env.call_method(self.record.as_obj(), "stop", "()V", &[])?;
        Ok(())
    }

    pub fn release(&self, env: &mut JNIEnv) -> Result<()> {
        env.call_method(self.record.as_obj(), "release", "()V", &[])?;
        Ok(())
    }

    /// A global reference to the underlying `AudioRecord`, for UI-thread stop.
    pub fn global_ref(&self) -> GlobalRef {
        self.record.clone()
    }
}

impl AndroidAudioPlayer {
    /// Create and start a streaming `AudioTrack` at `sample_rate`/`channels`.
    pub fn create(env: &mut JNIEnv, sample_rate: u32, channels: u16) -> Result<Self> {
        let channel_config = if channels <= 1 {
            CHANNEL_OUT_MONO
        } else {
            CHANNEL_OUT_STEREO
        };
        let class = env.find_class("android/media/AudioTrack")?;

        let min_buffer = env
            .call_static_method(
                &class,
                "getMinBufferSize",
                "(III)I",
                &[
                    JValue::Int(sample_rate as jint),
                    JValue::Int(channel_config),
                    JValue::Int(ENCODING_PCM_16BIT),
                ],
            )?
            .i()?;
        let buffer_size = (min_buffer * 2).max(4096);

        // Deprecated constructor, but the simplest one to drive from JNI and
        // still fully supported for streaming PCM playback.
        let track = env.new_object(
            &class,
            "(IIIIII)V",
            &[
                JValue::Int(STREAM_MUSIC),
                JValue::Int(sample_rate as jint),
                JValue::Int(channel_config),
                JValue::Int(ENCODING_PCM_16BIT),
                JValue::Int(buffer_size),
                JValue::Int(MODE_STREAM),
            ],
        )?;
        let track = env.new_global_ref(&track)?;
        env.call_method(track.as_obj(), "play", "()V", &[])?;
        Ok(Self { track })
    }

    /// Write interleaved `f32` samples as 16-bit PCM (blocking).
    pub fn write_f32(&self, env: &mut JNIEnv, samples: &[f32]) -> Result<()> {
        if samples.is_empty() {
            return Ok(());
        }
        let bytes: Vec<jbyte> = samples
            .iter()
            .flat_map(|&s| {
                let v = (s.clamp(-1.0, 1.0) * 32_767.0) as i16;
                v.to_le_bytes()
            })
            .map(|b| b as jbyte)
            .collect();
        let array: JByteArray = env.new_byte_array(bytes.len() as jint)?;
        env.set_byte_array_region(&array, 0, &bytes)?;
        let written = env
            .call_method(
                self.track.as_obj(),
                "write",
                "([BII)I",
                &[
                    JValue::Object(array.as_ref()),
                    JValue::Int(0),
                    JValue::Int(bytes.len() as jint),
                ],
            )?
            .i()?;
        if written != bytes.len() as jint {
            return Err(anyhow!(
                "AudioTrack write short: {written} of {} bytes",
                bytes.len()
            ));
        }
        Ok(())
    }

    /// Stop playback (also unblocks a pending blocking `write`).
    pub fn stop(&self, env: &mut JNIEnv) -> Result<()> {
        env.call_method(self.track.as_obj(), "stop", "()V", &[])?;
        Ok(())
    }

    pub fn release(&self, env: &mut JNIEnv) -> Result<()> {
        env.call_method(self.track.as_obj(), "release", "()V", &[])?;
        Ok(())
    }

    /// A global reference to the underlying `AudioTrack`, for UI-thread calls.
    pub fn global_ref(&self) -> GlobalRef {
        self.track.clone()
    }
}
