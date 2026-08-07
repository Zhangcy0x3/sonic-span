use cpal::traits::{DeviceTrait, HostTrait};

use crate::error::CaptureError;

/// A summary of one audio device, used by the `list-devices` CLI command.
#[derive(Debug, Clone)]
pub struct AudioDeviceInfo {
    /// Stable position in the enumeration order, used with `--device <index>`.
    pub index: usize,
    pub name: String,
    pub is_default: bool,
    pub sample_rate: u32,
    pub channels: u16,
    pub sample_format: String,
}

/// Enumerate all devices that can capture audio (including system loopback
/// devices such as "Stereo Mix", "BlackHole", or "Monitor of ...").
pub fn list_input_devices() -> Result<Vec<AudioDeviceInfo>, CaptureError> {
    let host = cpal::default_host();
    let default = host.default_input_device();
    let mut devices = Vec::new();

    for (index, device) in host
        .input_devices()
        .map_err(|e| CaptureError::DeviceQuery(e.to_string()))?
        .enumerate()
    {
        let name = device.name().unwrap_or_else(|_| "(unknown)".into());
        let cfg = device.default_input_config().ok();
        devices.push(AudioDeviceInfo {
            index,
            is_default: matches!(&default, Some(d) if device_name(d).as_deref() == Ok(name.as_str())),
            name,
            sample_rate: cfg.as_ref().map(|c| c.sample_rate().0).unwrap_or(0),
            channels: cfg.as_ref().map(|c| c.channels()).unwrap_or(0),
            sample_format: cfg
                .as_ref()
                .map(|c| format!("{:?}", c.sample_format()))
                .unwrap_or_default(),
        });
    }
    Ok(devices)
}

/// Enumerate all devices that can play audio.
pub fn list_output_devices() -> Result<Vec<AudioDeviceInfo>, CaptureError> {
    let host = cpal::default_host();
    let default = host.default_output_device();
    let mut devices = Vec::new();

    for (index, device) in host
        .output_devices()
        .map_err(|e| CaptureError::DeviceQuery(e.to_string()))?
        .enumerate()
    {
        let name = device.name().unwrap_or_else(|_| "(unknown)".into());
        let cfg = device.default_output_config().ok();
        devices.push(AudioDeviceInfo {
            index,
            is_default: matches!(&default, Some(d) if device_name(d).as_deref() == Ok(name.as_str())),
            name,
            sample_rate: cfg.as_ref().map(|c| c.sample_rate().0).unwrap_or(0),
            channels: cfg.as_ref().map(|c| c.channels()).unwrap_or(0),
            sample_format: cfg
                .as_ref()
                .map(|c| format!("{:?}", c.sample_format()))
                .unwrap_or_default(),
        });
    }
    Ok(devices)
}

/// Pick an input device by enumeration index, falling back to the system
/// default when `index` is `None`.
pub(crate) fn select_input_device(index: Option<usize>) -> Result<cpal::Device, CaptureError> {
    let host = cpal::default_host();
    match index {
        Some(index) => host
            .input_devices()
            .map_err(|e| CaptureError::DeviceQuery(e.to_string()))?
            .nth(index)
            .ok_or(CaptureError::DeviceNotFound { index }),
        None => host
            .default_input_device()
            .ok_or(CaptureError::NoDefaultInput),
    }
}

/// Pick an output device by enumeration index, falling back to the system
/// default when `index` is `None`.
pub(crate) fn select_output_device(index: Option<usize>) -> Result<cpal::Device, CaptureError> {
    let host = cpal::default_host();
    match index {
        Some(index) => host
            .output_devices()
            .map_err(|e| CaptureError::DeviceQuery(e.to_string()))?
            .nth(index)
            .ok_or(CaptureError::DeviceNotFound { index }),
        None => host
            .default_output_device()
            .ok_or(CaptureError::NoDefaultOutput),
    }
}

fn device_name(device: &cpal::Device) -> Result<String, String> {
    device.name().map_err(|e| e.to_string())
}
