use thiserror::Error;

/// Errors produced while enumerating devices or managing cpal streams.
#[derive(Debug, Error)]
pub enum CaptureError {
    #[error("no default audio input device available; run `list-devices` and pass --device <index>")]
    NoDefaultInput,

    #[error("no default audio output device available; run `list-devices` and pass --device <index>")]
    NoDefaultOutput,

    #[error("audio device #{index} not found; run `list-devices` to see available devices")]
    DeviceNotFound { index: usize },

    #[error("failed to query audio device: {0}")]
    DeviceQuery(String),

    #[error("unsupported sample format {0:?}")]
    UnsupportedSampleFormat(cpal::SampleFormat),

    #[error("failed to build audio stream: {0}")]
    BuildStream(String),

    #[error("failed to start audio stream: {0}")]
    StartStream(String),

    #[error("failed to stop audio stream: {0}")]
    StopStream(String),
}
