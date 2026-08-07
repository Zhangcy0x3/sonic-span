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

/// A streaming `AudioTrack` that accepts interleaved `f32` frames.
pub struct AndroidAudioPlayer {
    track: GlobalRef,
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
