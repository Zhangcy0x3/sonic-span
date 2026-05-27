use crate::errors::{AudioError, NetworkError};

pub struct AudioConfig {
    pub sample_rate: u32,
    pub channels: u16,
    // Other audio configuration parameters can be added here
}

pub trait AudioSource {
    /// Get current audio configuration, such as sample rate and number of channels
    fn config(&self) -> AudioConfig;

    /// Core library calls this method to "pull" raw audio data from the external source
    /// or use a callback mechanism to have the external source "push" data to the core library
    fn read_frames(&mut self, buffer: &mut [f32]) -> Result<usize, AudioError>;

    fn start(&mut self) -> Result<(), AudioError>;
    fn stop(&mut self) -> Result<(), AudioError>;
}

pub trait AudioSink {
    /// Get current audio configuration, such as sample rate and number of channels
    fn config(&self) -> AudioConfig;

    /// Core library calls this method to "push" PCM data to the audio output device
    fn write_frames(&mut self, buffer: &[f32]) -> Result<usize, AudioError>;

    fn start(&mut self) -> Result<(), AudioError>;
    fn stop(&mut self) -> Result<(), AudioError>;
}

#[async_trait::async_trait]
pub trait NetworkTransport {
    /// Send a packaged data frame (already includes header, sequence number, and compressed audio data)
    async fn send_packet(&mut self, payload: &[u8]) -> Result<(), NetworkError>;

    /// Receive data frames from the network
    async fn receive_packet(&mut self) -> Result<Vec<u8>, NetworkError>;

    /// Get current network status (latency, packet loss rate, etc.), used for core library to dynamically adjust strategies
    fn get_stats(&self) -> NetworkStats;
}

pub struct NetworkStats {
    pub latency_ms: u32,
    pub packet_loss_rate: f32,
    // Other network statistics can be added here
}
