//! Per-monitor capture via Windows.Graphics.Capture (WGC).
//!
//! # Threading
//!
//! - The frame pool is created with `CreateFreeThreaded`, so it needs no DispatcherQueue or
//!   message pump, and its `FrameArrived` event fires on a WGC worker thread.
//! - That handler does one thing: it keeps the *newest* frame in a mutex-protected slot and closes
//!   the one it replaces. This matters because with a small pool, an un-drained frame occupies a
//!   buffer and WGC drops newer content until it is released — draining only at tick time could
//!   hand back a frame up to one tick stale and miss the final state of a burst of changes.
//! - `MonitorCapturer` is `Send` (all its fields are; checked at compile time below) but its
//!   methods take `&mut self`: one owner thread at a time. The D3D11 immediate context is not
//!   safe for concurrent use; `&mut self` is what guarantees exclusivity.
//! - COM/WinRT: the constructing thread need not initialise COM. If it has not, windows-rs falls
//!   back to the process-wide implicit MTA when activating WinRT factories. Do not construct it on
//!   an STA thread you never pump; free-threaded objects do not need one.

use crate::error::{CaptureError, Result, WinContext};
use crate::monitor::MonitorHandle;
use rsrewind_core::BgraFrame;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use windows::Foundation::TypedEventHandler;
use windows::Graphics::Capture::{
    Direct3D11CaptureFrame, Direct3D11CaptureFramePool, GraphicsCaptureItem, GraphicsCaptureSession,
};
use windows::Graphics::DirectX::Direct3D11::IDirect3DDevice;
use windows::Graphics::DirectX::DirectXPixelFormat;
use windows::Graphics::SizeInt32;
use windows::Win32::Foundation::HMODULE;
use windows::Win32::Graphics::Direct3D::{
    D3D_DRIVER_TYPE, D3D_DRIVER_TYPE_HARDWARE, D3D_DRIVER_TYPE_WARP,
};
use windows::Win32::Graphics::Direct3D11::{
    D3D11_CPU_ACCESS_READ, D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_MAP_READ,
    D3D11_MAPPED_SUBRESOURCE, D3D11_SDK_VERSION, D3D11_TEXTURE2D_DESC, D3D11_USAGE_STAGING,
    D3D11CreateDevice, ID3D11Device, ID3D11DeviceContext, ID3D11Texture2D,
};
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC};
use windows::Win32::Graphics::Dxgi::{IDXGIAdapter, IDXGIDevice};
use windows::Win32::System::WinRT::Direct3D11::{
    CreateDirect3D11DeviceFromDXGIDevice, IDirect3DDxgiInterfaceAccess,
};
use windows::Win32::System::WinRT::Graphics::Capture::IGraphicsCaptureItemInterop;
use windows::core::{IInspectable, Interface};

/// Two buffers: one may be parked in our "newest frame" slot while WGC renders into the other.
const POOL_BUFFERS: i32 = 2;
const PIXEL_FORMAT: DirectXPixelFormat = DirectXPixelFormat::B8G8R8A8UIntNormalized;

/// Which D3D11 driver the capturer ended up on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Adapter {
    Hardware,
    /// Software rasteriser, used when no hardware device could be created (some VMs, broken
    /// drivers). Capture still works; the copy just costs more CPU.
    Warp,
}

type FrameSlot = Arc<Mutex<Option<Direct3D11CaptureFrame>>>;

/// Captures one monitor. See the module docs for the threading model.
pub struct MonitorCapturer {
    monitor: MonitorHandle,
    adapter: Adapter,
    device: ID3D11Device,
    context: ID3D11DeviceContext,
    item: GraphicsCaptureItem,
    pool: Direct3D11CaptureFramePool,
    session: GraphicsCaptureSession,
    pool_size: SizeInt32,
    staging: Option<Staging>,
    newest: FrameSlot,
    item_closed: Arc<AtomicBool>,
    frame_arrived_token: i64,
    closed_token: i64,
    border_disabled: bool,
    cursor_disabled: bool,
}

struct Staging {
    texture: ID3D11Texture2D,
    width: u32,
    height: u32,
}

// The daemon constructs capturers and moves them onto its capture thread.
const _: () = {
    const fn assert_send<T: Send>() {}
    assert_send::<MonitorCapturer>();
};

impl MonitorCapturer {
    /// Starts capturing `monitor`. The first frame typically arrives within one refresh.
    pub fn new(monitor: MonitorHandle) -> Result<Self> {
        if !GraphicsCaptureSession::IsSupported().ctx("GraphicsCaptureSession::IsSupported")? {
            return Err(CaptureError::Unsupported);
        }
        let (device, context, adapter) = create_d3d_device()?;
        let winrt_device = winrt_device(&device)?;

        let interop = windows::core::factory::<GraphicsCaptureItem, IGraphicsCaptureItemInterop>()
            .ctx("GraphicsCaptureItem interop factory")?;
        // SAFETY: the HMONITOR came from EnumDisplayMonitors; a stale one makes this return an
        // error (E_INVALIDARG), not undefined behaviour.
        let item: GraphicsCaptureItem = unsafe { interop.CreateForMonitor(monitor.hmonitor()) }
            .ctx("IGraphicsCaptureItemInterop::CreateForMonitor")?;
        let pool_size = item.Size().ctx("GraphicsCaptureItem::Size")?;

        let pool = Direct3D11CaptureFramePool::CreateFreeThreaded(
            &winrt_device,
            PIXEL_FORMAT,
            POOL_BUFFERS,
            pool_size,
        )
        .ctx("Direct3D11CaptureFramePool::CreateFreeThreaded")?;

        let newest: FrameSlot = Arc::new(Mutex::new(None));
        let slot = Arc::clone(&newest);
        let frame_arrived_token = pool
            .FrameArrived(
                &TypedEventHandler::<Direct3D11CaptureFramePool, IInspectable>::new(
                    move |sender, _| {
                        if let Some(pool) = sender.as_ref() {
                            park_newest(pool, &slot);
                        }
                        Ok(())
                    },
                ),
            )
            .ctx("Direct3D11CaptureFramePool::FrameArrived")?;

        let item_closed = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&item_closed);
        let closed_token = item
            .Closed(
                &TypedEventHandler::<GraphicsCaptureItem, IInspectable>::new(move |_, _| {
                    flag.store(true, Ordering::Release);
                    Ok(())
                }),
            )
            .ctx("GraphicsCaptureItem::Closed")?;

        let session = pool
            .CreateCaptureSession(&item)
            .ctx("Direct3D11CaptureFramePool::CreateCaptureSession")?;

        // Both settings are best effort: older builds lack the API, and the border may require
        // a capability an unpackaged app does not have. Neither is a reason not to record.
        let cursor_disabled = match session.SetIsCursorCaptureEnabled(false) {
            Ok(()) => true,
            Err(error) => {
                tracing::debug!(%error, "could not disable cursor capture; cursor will be in frames");
                false
            }
        };
        let border_disabled = match session.SetIsBorderRequired(false) {
            Ok(()) => true,
            Err(error) => {
                tracing::debug!(%error, "could not disable the capture border");
                false
            }
        };

        session
            .StartCapture()
            .ctx("GraphicsCaptureSession::StartCapture")?;
        tracing::debug!(
            ?adapter,
            width = pool_size.Width,
            height = pool_size.Height,
            cursor_disabled,
            border_disabled,
            "monitor capture started"
        );

        Ok(Self {
            monitor,
            adapter,
            device,
            context,
            item,
            pool,
            session,
            pool_size,
            staging: None,
            newest,
            item_closed,
            frame_arrived_token,
            closed_token,
            border_disabled,
            cursor_disabled,
        })
    }

    pub fn monitor(&self) -> MonitorHandle {
        self.monitor
    }

    pub fn adapter(&self) -> Adapter {
        self.adapter
    }

    /// Whether `IsBorderRequired = false` was accepted. Diagnostic only.
    pub fn border_disabled(&self) -> bool {
        self.border_disabled
    }

    /// Whether `IsCursorCaptureEnabled = false` was accepted. Diagnostic only.
    pub fn cursor_disabled(&self) -> bool {
        self.cursor_disabled
    }

    /// The newest frame Windows delivered since the previous call, or `None` if none arrived
    /// (WGC only produces frames when the monitor's content was recomposed).
    ///
    /// The returned frame is tightly packed (`stride == width * 4`), BGRA, top-down.
    ///
    /// Errors for which [`CaptureError::is_recoverable`] is true mean this capturer is finished:
    /// drop it and create a new one (after re-enumerating monitors).
    pub fn latest_frame(&mut self) -> Result<Option<BgraFrame>> {
        if self.item_closed.load(Ordering::Acquire) {
            return Err(CaptureError::ItemClosed);
        }
        // SAFETY: plain query on a live device. Ok = device healthy; Err carries the removal
        // reason (DXGI_ERROR_DEVICE_REMOVED/RESET/...), which classifies as recoverable.
        unsafe { self.device.GetDeviceRemovedReason() }.ctx("D3D11 device removed")?;

        // Pick up anything the handler has not seen yet (it may simply not have run).
        park_newest(&self.pool, &self.newest);
        let frame = self
            .newest
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        let Some(frame) = frame else {
            return Ok(None);
        };

        let copied = self.copy_frame(&frame);
        let content_size = frame.ContentSize();
        // Return the buffer to the pool as soon as the pixels are on the CPU side.
        if let Err(error) = frame.Close() {
            tracing::debug!(%error, "Direct3D11CaptureFrame::Close failed");
        }
        let copied = copied?;

        // Resolution / orientation change: the pool's textures keep their old size until it is
        // recreated. This frame was cropped to what both agree on; later ones will be full size.
        let content_size = content_size.ctx("Direct3D11CaptureFrame::ContentSize")?;
        if content_size != self.pool_size && content_size.Width > 0 && content_size.Height > 0 {
            tracing::debug!(
                old_width = self.pool_size.Width,
                old_height = self.pool_size.Height,
                new_width = content_size.Width,
                new_height = content_size.Height,
                "capture content size changed; recreating frame pool"
            );
            let winrt_device = winrt_device(&self.device)?;
            self.pool
                .Recreate(&winrt_device, PIXEL_FORMAT, POOL_BUFFERS, content_size)
                .ctx("Direct3D11CaptureFramePool::Recreate")?;
            self.pool_size = content_size;
        }

        Ok(Some(copied))
    }

    fn copy_frame(&mut self, frame: &Direct3D11CaptureFrame) -> Result<BgraFrame> {
        let surface = frame.Surface().ctx("Direct3D11CaptureFrame::Surface")?;
        let access: IDirect3DDxgiInterfaceAccess = surface
            .cast()
            .ctx("IDirect3DSurface -> IDirect3DDxgiInterfaceAccess")?;
        // SAFETY: requesting a COM interface by IID from a live surface.
        let texture: ID3D11Texture2D =
            unsafe { access.GetInterface() }.ctx("IDirect3DDxgiInterfaceAccess::GetInterface")?;

        let mut desc = D3D11_TEXTURE2D_DESC::default();
        // SAFETY: valid out-pointer for the call.
        unsafe { texture.GetDesc(&mut desc) };
        if desc.Format != DXGI_FORMAT_B8G8R8A8_UNORM {
            return Err(CaptureError::Invalid(format!(
                "frame texture format {:?}, expected B8G8R8A8_UNORM",
                desc.Format
            )));
        }
        let content = frame
            .ContentSize()
            .ctx("Direct3D11CaptureFrame::ContentSize")?;
        // The texture can be larger (pool not yet recreated after a shrink) or smaller (after a
        // grow) than the content. Copy what is valid in both.
        let width = desc.Width.min(content.Width.max(0) as u32);
        let height = desc.Height.min(content.Height.max(0) as u32);
        if width == 0 || height == 0 {
            return Err(CaptureError::Invalid(format!(
                "empty frame (texture {}x{}, content {}x{})",
                desc.Width, desc.Height, content.Width, content.Height
            )));
        }

        let staging = self.staging_for(&desc)?;
        // SAFETY: both textures belong to `self.device`, have identical size and format (the
        // staging texture was created from this desc), and the immediate context is used only
        // through `&mut self`.
        unsafe { self.context.CopyResource(&staging, &texture) };

        let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
        // SAFETY: staging texture is CPU-readable (USAGE_STAGING + CPU_ACCESS_READ); subresource
        // 0 exists (MipLevels = ArraySize = 1). Blocks until the copy above completes.
        unsafe {
            self.context
                .Map(&staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))
        }
        .ctx("ID3D11DeviceContext::Map")?;

        let result = read_mapped(&mapped, desc.Height, width, height);

        // SAFETY: matches the successful Map above; `read_mapped` has finished with the pointer.
        unsafe { self.context.Unmap(&staging, 0) };
        result
    }

    fn staging_for(&mut self, source: &D3D11_TEXTURE2D_DESC) -> Result<ID3D11Texture2D> {
        if let Some(staging) = &self.staging
            && staging.width == source.Width
            && staging.height == source.Height
        {
            return Ok(staging.texture.clone());
        }
        let desc = D3D11_TEXTURE2D_DESC {
            Width: source.Width,
            Height: source.Height,
            MipLevels: 1,
            ArraySize: 1,
            Format: source.Format,
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            Usage: D3D11_USAGE_STAGING,
            BindFlags: 0,
            CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
            MiscFlags: 0,
        };
        let mut texture = None;
        // SAFETY: valid descriptor and out-pointer for the call.
        unsafe { self.device.CreateTexture2D(&desc, None, Some(&mut texture)) }
            .ctx("ID3D11Device::CreateTexture2D(staging)")?;
        let texture = texture.ok_or_else(|| {
            CaptureError::Invalid("CreateTexture2D succeeded without a texture".into())
        })?;
        self.staging = Some(Staging {
            texture: texture.clone(),
            width: source.Width,
            height: source.Height,
        });
        Ok(texture)
    }
}

/// Copies the visible `width × height` region of a mapped BGRA texture into a packed frame.
fn read_mapped(
    mapped: &D3D11_MAPPED_SUBRESOURCE,
    texture_height: u32,
    width: u32,
    height: u32,
) -> Result<BgraFrame> {
    let row_bytes = width as usize * 4;
    let pitch = mapped.RowPitch as usize;
    if mapped.pData.is_null()
        || width == 0
        || height == 0
        || pitch < row_bytes
        || height > texture_height
    {
        return Err(CaptureError::Invalid(format!(
            "mapped texture unusable (pitch {pitch}, row {row_bytes}, null {})",
            mapped.pData.is_null()
        )));
    }
    let mapped_len = pitch * (height as usize - 1) + row_bytes;
    // SAFETY: a successful Map of a 2D subresource exposes `RowPitch * texture height` readable
    // bytes at pData. `mapped_len` stays within the first `height <= texture_height` rows, and
    // the slice is dropped before the caller unmaps.
    let source = unsafe { std::slice::from_raw_parts(mapped.pData as *const u8, mapped_len) };
    let mut pixels = Vec::with_capacity(row_bytes * height as usize);
    for y in 0..height as usize {
        pixels.extend_from_slice(&source[y * pitch..y * pitch + row_bytes]);
    }
    Ok(BgraFrame {
        width,
        height,
        stride: width * 4,
        pixels,
    })
}

/// Drains the pool, keeps only the newest frame in `slot`, and closes every frame it replaces so
/// its buffer goes straight back to WGC.
fn park_newest(pool: &Direct3D11CaptureFramePool, slot: &Mutex<Option<Direct3D11CaptureFrame>>) {
    let mut newest = None;
    // TryGetNextFrame reports "no frame" as an error (null result); a closed pool also errors.
    // Either way there is nothing more to take now.
    while let Ok(frame) = pool.TryGetNextFrame() {
        if let Some(older) = newest.replace(frame) {
            close_quietly(&older);
        }
    }
    let Some(frame) = newest else {
        return;
    };
    // The WGC handler and `latest_frame` can both drain concurrently; whichever locks last must
    // not overwrite a newer parked frame with an older one. The mutex is deliberately not held
    // across TryGetNextFrame, so we never wait on our lock while WGC might hold one of its own.
    let mut parked = slot.lock().unwrap_or_else(PoisonError::into_inner);
    let parked_is_newer = match (parked.as_ref(), frame_time(&frame)) {
        (Some(existing), Some(ours)) => frame_time(existing).is_some_and(|t| t > ours),
        _ => false,
    };
    if parked_is_newer {
        close_quietly(&frame);
    } else if let Some(previous) = parked.replace(frame) {
        close_quietly(&previous);
    }
}

fn frame_time(frame: &Direct3D11CaptureFrame) -> Option<i64> {
    frame.SystemRelativeTime().ok().map(|t| t.Duration)
}

fn close_quietly(frame: &Direct3D11CaptureFrame) {
    if let Err(error) = frame.Close() {
        tracing::debug!(%error, "Direct3D11CaptureFrame::Close failed");
    }
}

fn create_d3d_device() -> Result<(ID3D11Device, ID3D11DeviceContext, Adapter)> {
    match create_device_of(D3D_DRIVER_TYPE_HARDWARE) {
        Ok((device, context)) => Ok((device, context, Adapter::Hardware)),
        Err(hardware_error) => {
            tracing::debug!(%hardware_error, "hardware D3D11 device failed; trying WARP");
            let (device, context) = create_device_of(D3D_DRIVER_TYPE_WARP)
                .ctx("D3D11CreateDevice (hardware and WARP)")?;
            Ok((device, context, Adapter::Warp))
        }
    }
}

fn create_device_of(
    driver: D3D_DRIVER_TYPE,
) -> windows::core::Result<(ID3D11Device, ID3D11DeviceContext)> {
    let mut device = None;
    let mut context = None;
    // SAFETY: valid out-pointers for the call. BGRA support is required for WGC interop. The
    // device is created without D3D11_CREATE_DEVICE_SINGLETHREADED because WGC itself touches it
    // from its own threads.
    unsafe {
        D3D11CreateDevice(
            None::<&IDXGIAdapter>,
            driver,
            HMODULE::default(),
            D3D11_CREATE_DEVICE_BGRA_SUPPORT,
            None,
            D3D11_SDK_VERSION,
            Some(&mut device),
            None,
            Some(&mut context),
        )?;
    }
    match (device, context) {
        (Some(device), Some(context)) => Ok((device, context)),
        _ => Err(windows::core::Error::from_hresult(
            windows::Win32::Foundation::E_POINTER,
        )),
    }
}

/// The WinRT `IDirect3DDevice` wrapper WGC needs. Derived on demand instead of stored, because
/// the WinRT interface type is not marked `Send` by windows-rs and we want `MonitorCapturer` to
/// be `Send` without an `unsafe impl`.
fn winrt_device(device: &ID3D11Device) -> Result<IDirect3DDevice> {
    let dxgi: IDXGIDevice = device.cast().ctx("ID3D11Device -> IDXGIDevice")?;
    // SAFETY: `dxgi` is a live DXGI device; the function returns a new reference.
    let inspectable = unsafe { CreateDirect3D11DeviceFromDXGIDevice(&dxgi) }
        .ctx("CreateDirect3D11DeviceFromDXGIDevice")?;
    inspectable.cast().ctx("IInspectable -> IDirect3DDevice")
}

impl Drop for MonitorCapturer {
    fn drop(&mut self) {
        // Unhook first so the WGC worker stops parking frames into the slot we are clearing.
        if let Err(error) = self.pool.RemoveFrameArrived(self.frame_arrived_token) {
            tracing::debug!(%error, "RemoveFrameArrived failed");
        }
        if let Err(error) = self.item.RemoveClosed(self.closed_token) {
            tracing::debug!(%error, "RemoveClosed failed");
        }
        let parked = self
            .newest
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        if let Some(frame) = parked {
            close_quietly(&frame);
        }
        if let Err(error) = self.session.Close() {
            tracing::debug!(%error, "GraphicsCaptureSession::Close failed");
        }
        if let Err(error) = self.pool.Close() {
            tracing::debug!(%error, "Direct3D11CaptureFramePool::Close failed");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_padded_rows() -> std::result::Result<(), Box<dyn std::error::Error>> {
        // 3×2 visible pixels in a 4-pixel-pitch texture (16 bytes/row), plus padding garbage.
        let mut buffer = vec![0xEEu8; 16 * 2];
        for y in 0..2 {
            for x in 0..3 {
                let i = y * 16 + x * 4;
                buffer[i..i + 4].copy_from_slice(&[x as u8, y as u8, 7, 255]);
            }
        }
        let mapped = D3D11_MAPPED_SUBRESOURCE {
            pData: buffer.as_mut_ptr().cast(),
            RowPitch: 16,
            DepthPitch: 32,
        };
        let frame = read_mapped(&mapped, 2, 3, 2)?;
        assert_eq!((frame.width, frame.height, frame.stride), (3, 2, 12));
        assert!(frame.is_well_formed());
        assert!(!frame.pixels.contains(&0xEE));
        assert_eq!(&frame.pixels[12..16], &[0, 1, 7, 255]);
        Ok(())
    }

    #[test]
    fn rejects_short_pitch_and_null() {
        let mut buffer = vec![0u8; 64];
        let short = D3D11_MAPPED_SUBRESOURCE {
            pData: buffer.as_mut_ptr().cast(),
            RowPitch: 8,
            DepthPitch: 16,
        };
        assert!(read_mapped(&short, 2, 3, 2).is_err());
        let null = D3D11_MAPPED_SUBRESOURCE {
            pData: std::ptr::null_mut(),
            RowPitch: 16,
            DepthPitch: 32,
        };
        assert!(read_mapped(&null, 2, 3, 2).is_err());
        let too_tall = D3D11_MAPPED_SUBRESOURCE {
            pData: buffer.as_mut_ptr().cast(),
            RowPitch: 16,
            DepthPitch: 32,
        };
        assert!(read_mapped(&too_tall, 2, 3, 3).is_err());
    }
}
