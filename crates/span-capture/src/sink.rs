use std::sync::{Arc, Mutex};

use cpal::traits::{DeviceTrait, StreamTrait};
use cpal::{BufferSize, SampleRate, StreamConfig};
use span_core::buffer::AudioRingBuffer;
use span_core::errors::AudioError;
use span_core::traits::{AudioConfig, AudioSink};

use crate::device::select_output_device;
use crate::error::CaptureError;

/// Default capacity of the internal ring buffer, in audio frames.
pub const DEFAULT_BUFFER_CAPACITY_FRAMES: usize = 262_144;

/// A [`cpal`]-backed audio output sink.
///
/// cpal pulls audio through a callback, so this type bridges the push-based
/// [`AudioSink`] trait into cpal: [`AudioSink::write_frames`] writes into a
/// lock-free ring buffer and the audio thread pops from it.
pub struct CpalSink {
    device: cpal::Device,
    device_name: String,
    config: AudioConfig,
    sample_format: cpal::SampleFormat,
    buffer: Arc<AudioRingBuffer>,
    scratch: Arc<Mutex<Vec<f32>>>,
    stream: Option<cpal::Stream>,
}

impl CpalSink {
    /// Open an output device (by `list-devices` index, or the system default).
    ///
    /// The stream is configured for `sample_rate`/`channels` when the device
    /// supports them; otherwise the device's default configuration is used.
    pub fn new(
        device_index: Option<usize>,
        sample_rate: u32,
        channels: u16,
    ) -> Result<Self, CaptureError> {
        let device = select_output_device(device_index)?;
        let device_name = device
            .name()
            .map_err(|e| CaptureError::DeviceQuery(e.to_string()))?;
        let (stream_config, sample_format) = choose_output_config(&device, sample_rate, channels)?;

        Ok(Self {
            config: AudioConfig {
                sample_rate: stream_config.sample_rate.0,
                channels: stream_config.channels,
            },
            device,
            device_name,
            sample_format,
            buffer: Arc::new(AudioRingBuffer::new(DEFAULT_BUFFER_CAPACITY_FRAMES)),
            scratch: Arc::new(Mutex::new(Vec::new())),
            stream: None,
        })
    }

    /// Name of the device audio is played on.
    pub fn device_name(&self) -> &str {
        &self.device_name
    }
}

impl AudioSink for CpalSink {
    fn config(&self) -> AudioConfig {
        self.config.clone()
    }

    fn write_frames(&mut self, input: &[f32]) -> Result<usize, AudioError> {
        Ok(self.buffer.push(input))
    }

    fn start(&mut self) -> Result<(), AudioError> {
        if self.stream.is_some() {
            return Ok(());
        }
        let stream_config = StreamConfig {
            channels: self.config.channels as cpal::ChannelCount,
            sample_rate: SampleRate(self.config.sample_rate),
            buffer_size: BufferSize::Default,
        };

        let stream = build_output_stream(
            &self.device,
            &stream_config,
            self.sample_format,
            Arc::clone(&self.buffer),
            Arc::clone(&self.scratch),
        )
        .map_err(|e| AudioError::OutputError(e.to_string()))?;
        stream
            .play()
            .map_err(|e| AudioError::OutputError(format!("failed to start stream: {e}")))?;
        self.stream = Some(stream);
        Ok(())
    }

    fn stop(&mut self) -> Result<(), AudioError> {
        self.stream = None;
        Ok(())
    }
}

/// Prefer the requested configuration; fall back to the device default when
/// unsupported. Returns the stream config and its sample format.
fn choose_output_config(
    device: &cpal::Device,
    sample_rate: u32,
    channels: u16,
) -> Result<(StreamConfig, cpal::SampleFormat), CaptureError> {
    let default = device
        .default_output_config()
        .map_err(|e| CaptureError::DeviceQuery(e.to_string()))?;

    let requested = device
        .supported_output_configs()
        .map_err(|e| CaptureError::DeviceQuery(e.to_string()))?
        .find(|range| {
            range.channels() == channels
                && range.min_sample_rate().0 <= sample_rate
                && sample_rate <= range.max_sample_rate().0
        });

    match requested {
        Some(range) => Ok((
            StreamConfig {
                channels,
                sample_rate: SampleRate(sample_rate),
                buffer_size: BufferSize::Default,
            },
            range.sample_format(),
        )),
        None => Ok((default.config(), default.sample_format())),
    }
}

/// Build a dynamically-typed cpal output stream that pops f32 frames from the
/// ring buffer and converts them to the device's sample format.
fn build_output_stream(
    device: &cpal::Device,
    config: &StreamConfig,
    sample_format: cpal::SampleFormat,
    buffer: Arc<AudioRingBuffer>,
    scratch: Arc<Mutex<Vec<f32>>>,
) -> Result<cpal::Stream, CaptureError> {
    let err_fn = |err| eprintln!("[span-capture] output stream error: {err}");

    match sample_format {
        cpal::SampleFormat::F32 => device
            .build_output_stream::<f32, _, _>(
                config,
                move |data, _| {
                    let n = buffer.pop(data);
                    for sample in &mut data[n..] {
                        *sample = 0.0;
                    }
                },
                err_fn,
                None,
            )
            .map_err(|e| CaptureError::BuildStream(e.to_string())),

        cpal::SampleFormat::I16 => device
            .build_output_stream::<i16, _, _>(
                config,
                move |data, _| {
                    let mut src = scratch.lock().expect("scratch lock poisoned");
                    src.resize(data.len(), 0.0);
                    let n = buffer.pop(&mut src);
                    for (dst, &s) in data.iter_mut().zip(src.iter()) {
                        *dst = (s.clamp(-1.0, 1.0) * 32_767.0) as i16;
                    }
                    for dst in &mut data[n..] {
                        *dst = 0;
                    }
                },
                err_fn,
                None,
            )
            .map_err(|e| CaptureError::BuildStream(e.to_string())),

        cpal::SampleFormat::U16 => device
            .build_output_stream::<u16, _, _>(
                config,
                move |data, _| {
                    let mut src = scratch.lock().expect("scratch lock poisoned");
                    src.resize(data.len(), 0.0);
                    let n = buffer.pop(&mut src);
                    for (dst, &s) in data.iter_mut().zip(src.iter()) {
                        let v = (s.clamp(-1.0, 1.0) * 32_767.0) as i16 as u16;
                        *dst = v ^ 0x8000;
                    }
                    for dst in &mut data[n..] {
                        *dst = 0x8000;
                    }
                },
                err_fn,
                None,
            )
            .map_err(|e| CaptureError::BuildStream(e.to_string())),

        cpal::SampleFormat::I32 => device
            .build_output_stream::<i32, _, _>(
                config,
                move |data, _| {
                    let mut src = scratch.lock().expect("scratch lock poisoned");
                    src.resize(data.len(), 0.0);
                    let n = buffer.pop(&mut src);
                    for (dst, &s) in data.iter_mut().zip(src.iter()) {
                        *dst = (s.clamp(-1.0, 1.0) * 2_147_483_647.0) as i32;
                    }
                    for dst in &mut data[n..] {
                        *dst = 0;
                    }
                },
                err_fn,
                None,
            )
            .map_err(|e| CaptureError::BuildStream(e.to_string())),

        other => Err(CaptureError::UnsupportedSampleFormat(other)),
    }
}
