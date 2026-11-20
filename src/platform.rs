//! Plattform-Schnittstelle. Unter Windows wird `win.rs` verwendet, sonst liefern die Stubs einen Laufzeitfehler.

use crate::policy::Rect;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawWindow {
    pub hwnd: i64,
    pub process: String,
    pub title: String,
    pub rect: Rect,
    pub visible: bool,
    pub minimized: bool,
    pub cloaked: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MonitorInfo {
    pub handle: i64,
    pub rect: Rect,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    pub width: u32,
    pub height: u32,
    /// BGRA8, premultiplied (so wie WGC es liefert).
    pub bgra: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(not(windows), allow(dead_code))]
pub enum CaptureError {
    Denied,
    NoFrame,
    Minimized,
    Runtime(String),
}

#[cfg(windows)]
pub use crate::win::*;

#[cfg(not(windows))]
mod stub {
    use super::*;
    use std::time::Duration;

    const MESSAGE: &str = "desktop-snap ist nur unter Windows lauffähig.";

    pub fn set_dpi_awareness() -> bool {
        false
    }

    pub fn wgc_supported() -> bool {
        false
    }

    pub fn border_control_supported() -> bool {
        false
    }

    pub fn list_windows() -> Result<Vec<RawWindow>, String> {
        Err(MESSAGE.into())
    }

    pub fn list_monitors() -> Result<Vec<MonitorInfo>, String> {
        Err(MESSAGE.into())
    }

    pub fn capture_window(_hwnd: i64, _timeout: Duration) -> Result<Frame, CaptureError> {
        Err(CaptureError::Runtime(MESSAGE.into()))
    }

    pub fn capture_monitor(_handle: i64, _timeout: Duration) -> Result<Frame, CaptureError> {
        Err(CaptureError::Runtime(MESSAGE.into()))
    }
}

#[cfg(not(windows))]
pub use stub::*;
