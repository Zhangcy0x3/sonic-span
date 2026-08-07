//! Platform audio capture (system loopback) and playback built on [`cpal`].
//!
//! [`source::CpalLoopbackSource`] implements [`AudioSource`] and captures the
//! audio that the system is currently playing. [`sink::CpalSink`] implements
//! [`AudioSink`] and plays PCM frames on an output device.
//!
//! [`AudioSource`]: span_core::traits::AudioSource
//! [`AudioSink`]: span_core::traits::AudioSink

pub mod device;
pub mod error;
pub mod sink;
pub mod source;
