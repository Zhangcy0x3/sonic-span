use std::sync::{Arc, Mutex};

use cpal::traits::{DeviceTrait, StreamTrait};
use span_core::buffer::AudioRingBuffer;
use span_core::errors::AudioError;
use span_core::traits::{AudioConfig, AudioSource};

use crate::device::select_input_device;
use crate::error::CaptureError;

/// Default capacity of the internal ring buffer, in audio frames (samples
/// across all channels). At 48 kHz stereo this is ~2.7 seconds of audio.
pub const DEFAULT_BUFFER_CAPACITY_FRAMES: usize = 262_144;

/// A [`cpal`]-backed system audio capture source.
///
/// cpal delivers audio through a callback, so this type bridges the push-based
/// capture into the pull-based [`AudioSource`] trait via a lock-free ring
/// buffer: the audio thread writes, [`AudioSource::read_frames`] reads.
pub struct CpalLoopbackSource {
    device: cpal::Device,
    device_name: String,
    config: AudioConfig,
    supported_config: cpal::SupportedStreamConfig,
    buffer: Arc<AudioRingBuffer>,
    scratch: Arc<Mutex<Vec<f32>>>,
    stream: Option<cpal::Stream>,
}

impl CpalLoopbackSource {
    /// Open a capture device (by `list-devices` index, or the system default).
    pub fn new(
        device_index: Option<usize>,
        buffer_capacity_frames: usize,
    ) -> Result<Self, CaptureError> {
        let device = select_input_device(device_index)?;
        let device_name = device
            .name()
            .map_err(|e| CaptureError::DeviceQuery(e.to_string()))?;
        let supported_config = device
            .default_input_config()
            .map_err(|e| CaptureError::DeviceQuery(e.to_string()))?;
        let config = AudioConfig {
            sample_rate: supported_config.sample_rate().0,
            channels: supported_config.channels() as u16,
        };
        Ok(Self {
            device,
            device_name,
            config,
            supported_config,
            buffer: Arc::new(AudioRingBuffer::new(buffer_capacity_frames)),
            scratch: Arc::new(Mutex::new(Vec::new())),
            stream: None,
        })
    }

    /// Name of the device being captured from.
    pub fn device_name(&self) -> &str {
        &self.device_name
    }
}

impl AudioSource for CpalLoopbackSource {
    fn config(&self) -> AudioConfig {
        self.config.clone()
    }

    fn read_frames(&mut self, output: &mut [f32]) -> Result<usize, AudioError> {
        Ok(self.buffer.pop(output))
    }

    fn start(&mut self) -> Result<(), AudioError> {
        if self.stream.is_some() {
            return Ok(());
        }
        let stream_config = self.supported_config.config();
        let sample_format = self.supported_config.sample_format();
        let stream = build_input_stream(
            &self.device,
            &stream_config,
            sample_format,
            Arc::clone(&self.buffer),
            Arc::clone(&self.scratch),
        )
        .map_err(|e| AudioError::SourceError(e.to_string()))?;
        stream
            .play()
            .map_err(|e| AudioError::SourceError(format!("failed to start stream: {e}")))?;
        self.stream = Some(stream);
        Ok(())
    }

    fn stop(&mut self) -> Result<(), AudioError> {
        self.stream = None;
        Ok(())
    }
}

/// Build a dynamically-typed cpal input stream that converts samples to f32
/// and pushes them into the ring buffer.
fn build_input_stream(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    sample_format: cpal::SampleFormat,
    buffer: Arc<AudioRingBuffer>,
    scratch: Arc<Mutex<Vec<f32>>>,
) -> Result<cpal::Stream, CaptureError> {
    let err_fn = |err| eprintln!("[span-capture] input stream error: {err}");

    match sample_format {
        cpal::SampleFormat::F32 => device
            .build_input_stream::<f32, _, _>(
                config,
                move |data, _| {
                    buffer.push(data);
                },
                err_fn,
                None,
            )
            .map_err(|e| CaptureError::BuildStream(e.to_string())),

        cpal::SampleFormat::I16 => device
            .build_input_stream::<i16, _, _>(
                config,
                move |data, _| {
                    let mut dst = scratch.lock().expect("scratch lock poisoned");
                    dst.clear();
                    dst.extend(data.iter().map(|&s| s as f32 / 32_768.0));
                    buffer.push(&dst);
                },
                err_fn,
                None,
            )
            .map_err(|e| CaptureError::BuildStream(e.to_string())),

        cpal::SampleFormat::U16 => device
            .build_input_stream::<u16, _, _>(
                config,
                move |data, _| {
                    let mut dst = scratch.lock().expect("scratch lock poisoned");
                    dst.clear();
                    dst.extend(data.iter().map(|&s| (s as f32 - 32_768.0) / 32_768.0));
                    buffer.push(&dst);
                },
                err_fn,
                None,
            )
            .map_err(|e| CaptureError::BuildStream(e.to_string())),

        cpal::SampleFormat::I32 => device
            .build_input_stream::<i32, _, _>(
                config,
                move |data, _| {
                    let mut dst = scratch.lock().expect("scratch lock poisoned");
                    dst.clear();
                    dst.extend(data.iter().map(|&s| s as f32 / 2_147_483_648.0));
                    buffer.push(&dst);
                },
                err_fn,
                None,
            )
            .map_err(|e| CaptureError::BuildStream(e.to_string())),

        other => Err(CaptureError::UnsupportedSampleFormat(other)),
    }
}
