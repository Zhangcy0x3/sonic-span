#[derive(Debug, Error)]
pub enum AudioError {
    #[error("Audio source error: {0}")]
    SourceError(String),
    
    #[error("Audio processing error: {0}")]
    ProcessingError(String),
    
    #[error("Audio output error: {0}")]
    OutputError(String),
    
    // Other audio-related errors can be added here
}

#[derive(Debug, Error)]
pub enum NetworkError {
    #[error("Network send error: {0}")]
    SendError(String),
    #[error("Network receive error: {0}")]
    ReceiveError(String),
    #[error("Network stats error: {0}")]
    StatsError(String),
    
    // Other network-related errors can be added here
} 