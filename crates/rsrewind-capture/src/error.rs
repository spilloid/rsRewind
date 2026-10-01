use windows::Win32::Foundation::{E_ACCESSDENIED, RO_E_CLOSED};
use windows::Win32::Graphics::Dxgi::{
    DXGI_ERROR_ACCESS_LOST, DXGI_ERROR_DEVICE_HUNG, DXGI_ERROR_DEVICE_REMOVED,
    DXGI_ERROR_DEVICE_RESET, DXGI_ERROR_DRIVER_INTERNAL_ERROR,
};
use windows::core::HRESULT;

pub type Result<T, E = CaptureError> = std::result::Result<T, E>;

#[derive(Debug, thiserror::Error)]
pub enum CaptureError {
    /// Windows.Graphics.Capture is unavailable (pre-1903 Windows, or a session type that does not
    /// support it). Recreating will not help.
    #[error("Windows.Graphics.Capture is not supported in this session")]
    Unsupported,

    /// The capture pipeline is dead but a new `MonitorCapturer` may well work: the GPU was
    /// removed/reset (driver update, TDR, RDP reconnect), or the capture item closed (monitor
    /// unplugged, display mode change). The daemon should drop this capturer, count the event,
    /// re-enumerate monitors and recreate.
    #[error("capture must be recreated ({context}): {source}")]
    Recoverable {
        context: &'static str,
        #[source]
        source: windows::core::Error,
    },

    /// The capture item raised its `Closed` event (monitor gone). Recoverable by recreation, if
    /// the monitor still exists.
    #[error("capture item was closed (monitor disconnected or display reconfigured)")]
    ItemClosed,

    /// Any other Windows failure. Not expected to fix itself by recreating the capturer.
    #[error("{context}: {source}")]
    Windows {
        context: &'static str,
        #[source]
        source: windows::core::Error,
    },

    /// A Windows call succeeded but returned something we cannot use (e.g. a row pitch smaller
    /// than a row). Defensive; indicates a driver or API contract violation.
    #[error("unexpected capture data: {0}")]
    Invalid(String),
}

impl CaptureError {
    /// `true` when dropping this capturer and constructing a new one is the right response.
    pub fn is_recoverable(&self) -> bool {
        matches!(self, Self::Recoverable { .. } | Self::ItemClosed)
    }

    /// Wraps a Windows error, classifying it as recoverable or not by its HRESULT.
    pub(crate) fn win(context: &'static str, source: windows::core::Error) -> Self {
        if is_recoverable_hresult(source.code()) {
            Self::Recoverable { context, source }
        } else {
            Self::Windows { context, source }
        }
    }
}

/// HRESULTs after which the D3D device or the capture item is unusable but a freshly created
/// capturer is expected to work.
pub fn is_recoverable_hresult(code: HRESULT) -> bool {
    [
        DXGI_ERROR_DEVICE_REMOVED,
        DXGI_ERROR_DEVICE_RESET,
        DXGI_ERROR_DEVICE_HUNG,
        DXGI_ERROR_DRIVER_INTERNAL_ERROR,
        DXGI_ERROR_ACCESS_LOST,
        // Using a closed WinRT object (frame pool / session closed underneath us).
        RO_E_CLOSED,
    ]
    .contains(&code)
}

/// Extension to attach a context string and classify in one step.
pub(crate) trait WinContext<T> {
    fn ctx(self, context: &'static str) -> Result<T>;
}

impl<T> WinContext<T> for windows::core::Result<T> {
    fn ctx(self, context: &'static str) -> Result<T> {
        self.map_err(|e| CaptureError::win(context, e))
    }
}

/// `E_ACCESSDENIED` when creating a capture item usually means the session cannot capture
/// (secure desktop, locked workstation). Exposed so callers can tell it apart in logs.
pub fn is_access_denied(error: &CaptureError) -> bool {
    match error {
        CaptureError::Windows { source, .. } | CaptureError::Recoverable { source, .. } => {
            source.code() == E_ACCESSDENIED
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn device_loss_is_recoverable() {
        for code in [
            DXGI_ERROR_DEVICE_REMOVED,
            DXGI_ERROR_DEVICE_RESET,
            DXGI_ERROR_DEVICE_HUNG,
            RO_E_CLOSED,
        ] {
            let err = CaptureError::win("test", windows::core::Error::from_hresult(code));
            assert!(err.is_recoverable(), "{code:?}");
        }
        assert!(CaptureError::ItemClosed.is_recoverable());
    }

    #[test]
    fn other_failures_are_fatal() {
        let err = CaptureError::win("test", windows::core::Error::from_hresult(E_ACCESSDENIED));
        assert!(!err.is_recoverable());
        assert!(is_access_denied(&err));
        assert!(!CaptureError::Unsupported.is_recoverable());
        assert!(!CaptureError::Invalid("x".into()).is_recoverable());
    }
}
