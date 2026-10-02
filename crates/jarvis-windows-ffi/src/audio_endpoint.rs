//! Narrow Core Audio adapter for the current user's default render endpoint.
//! No endpoint ID, device path, or COM interface comes from a request.

use std::fmt;
use std::ptr::null;

use windows::Win32::Media::Audio::Endpoints::IAudioEndpointVolume;
use windows::Win32::Media::Audio::{eConsole, eRender, IMMDeviceEnumerator, MMDeviceEnumerator};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_ALL, COINIT_MULTITHREADED,
};

#[derive(Debug)]
pub enum AudioEndpointError {
    InvalidPercent,
    Windows(windows::core::Error),
    ReadbackMismatch { requested: u8, observed: f32 },
}

impl fmt::Display for AudioEndpointError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidPercent => formatter.write_str("volume must be from 0 through 100"),
            Self::Windows(error) => write!(formatter, "Core Audio failed: {error}"),
            Self::ReadbackMismatch {
                requested,
                observed,
            } => write!(
                formatter,
                "volume readback did not match requested {requested}% (observed scalar {observed})"
            ),
        }
    }
}

impl std::error::Error for AudioEndpointError {}

struct ComApartment;

impl Drop for ComApartment {
    fn drop(&mut self) {
        // SAFETY: Created only after a successful CoInitializeEx on this same
        // synchronous monitor thread; each successful call must be balanced.
        unsafe { CoUninitialize() };
    }
}

/// Sets and reads back the master volume of the default console render
/// endpoint. The readback checks dispatch, not audible output.
pub fn set_default_master_volume(percent: u8) -> Result<f32, AudioEndpointError> {
    if percent > 100 {
        return Err(AudioEndpointError::InvalidPercent);
    }
    // SAFETY: COM is initialized and released on the same thread. All COM
    // pointers are owned by windows-rs wrappers and remain within this call.
    unsafe {
        CoInitializeEx(None, COINIT_MULTITHREADED)
            .ok()
            .map_err(AudioEndpointError::Windows)?;
        let _apartment = ComApartment;
        let enumerator: IMMDeviceEnumerator =
            CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)
                .map_err(AudioEndpointError::Windows)?;
        let device = enumerator
            .GetDefaultAudioEndpoint(eRender, eConsole)
            .map_err(AudioEndpointError::Windows)?;
        let volume: IAudioEndpointVolume = device
            .Activate(CLSCTX_ALL, None)
            .map_err(AudioEndpointError::Windows)?;
        let target = f32::from(percent) / 100.0;
        volume
            .SetMasterVolumeLevelScalar(target, null())
            .map_err(AudioEndpointError::Windows)?;
        let observed = volume
            .GetMasterVolumeLevelScalar()
            .map_err(AudioEndpointError::Windows)?;
        if (observed - target).abs() > 0.005 {
            return Err(AudioEndpointError::ReadbackMismatch {
                requested: percent,
                observed,
            });
        }
        Ok(observed)
    }
}
