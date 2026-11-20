//! Windows-Schicht: Fensterliste, Monitore und WGC-only Capture. Keine GDI-, BitBlt-, PrintWindow- oder DXGI-Duplikation.
#![cfg(windows)]

use crate::platform::{CaptureError, Frame, MonitorInfo, RawWindow};
use crate::policy::Rect;
use std::path::Path;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use windows::core::{Interface, BOOL, HSTRING};
use windows::Foundation::{Metadata::ApiInformation, TypedEventHandler};
use windows::Graphics::Capture::{Direct3D11CaptureFramePool, Direct3D11CaptureFrame, GraphicsCaptureItem, GraphicsCaptureSession};
use windows::Graphics::DirectX::DirectXPixelFormat;
use windows::Graphics::DirectX::Direct3D11::IDirect3DDevice;
use windows::Win32::Foundation::{CloseHandle, E_ACCESSDENIED, HMODULE, HWND, LPARAM, RECT};
use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_HARDWARE;
use windows::Win32::Graphics::Direct3D11::{
    D3D11CreateDevice, ID3D11Device, ID3D11DeviceContext, ID3D11Texture2D, D3D11_CPU_ACCESS_READ,
    D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_MAP_READ, D3D11_MAPPED_SUBRESOURCE, D3D11_SDK_VERSION,
    D3D11_TEXTURE2D_DESC, D3D11_USAGE_STAGING,
};
use windows::Win32::Graphics::Dwm::{DwmGetWindowAttribute, DWMWA_CLOAKED, DWMWA_EXTENDED_FRAME_BOUNDS};
use windows::Win32::Graphics::Dxgi::IDXGIDevice;
use windows::Win32::Graphics::Gdi::{EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITORINFO};
use windows::Win32::System::Threading::{
    OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::Win32::System::WinRT::Direct3D11::{CreateDirect3D11DeviceFromDXGIDevice, IDirect3DDxgiInterfaceAccess};
use windows::Win32::System::WinRT::Graphics::Capture::IGraphicsCaptureItemInterop;
use windows::Win32::System::WinRT::{RoInitialize, RO_INIT_MULTITHREADED};
use windows::Win32::UI::HiDpi::{
    GetAwarenessFromDpiAwarenessContext, GetThreadDpiAwarenessContext, SetProcessDpiAwarenessContext,
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, PROCESS_PER_MONITOR_DPI_AWARE,
};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetWindowTextLengthW, GetWindowTextW, GetWindowThreadProcessId, IsIconic, IsWindowVisible,
};

const PROPERTY_TYPE: &str = "Windows.Graphics.Capture.GraphicsCaptureSession";

fn map_err(error: windows::core::Error) -> CaptureError {
    if error.code() == E_ACCESSDENIED {
        CaptureError::Denied
    } else {
        CaptureError::Runtime(format!("{error}"))
    }
}

fn ensure_ro_init() {
    // S_FALSE / RPC_E_CHANGED_MODE sind hier unkritisch: der Thread ist dann bereits initialisiert.
    let _ = unsafe { RoInitialize(RO_INIT_MULTITHREADED) };
}

/// Setzt PerMonitorV2 und liefert zurück, ob PerMonitorV2 danach tatsächlich aktiv ist.
/// Das Manifest setzt denselben Wert bereits; dann schlägt der Aufruf mit Zugriffsverweigerung fehl,
/// und die Aussage stützt sich allein auf den tatsächlichen Zustand.
pub fn set_dpi_awareness() -> bool {
    let _ = unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
    // Der Kontext ist ein Handle; die Awareness wird über die Umrechnung gelesen, nicht über den Rohwert.
    let current = unsafe { GetThreadDpiAwarenessContext() };
    let awareness = unsafe { GetAwarenessFromDpiAwarenessContext(current) };
    awareness.0 == PROCESS_PER_MONITOR_DPI_AWARE.0
}

pub fn wgc_supported() -> bool {
    GraphicsCaptureSession::IsSupported().unwrap_or(false)
}

pub fn border_control_supported() -> bool {
    ApiInformation::IsPropertyPresent(&HSTRING::from(PROPERTY_TYPE), &HSTRING::from("IsBorderRequired")).unwrap_or(false)
}

unsafe extern "system" fn collect_window(hwnd: HWND, lparam: LPARAM) -> BOOL {
    let list = &mut *(lparam.0 as *mut Vec<HWND>);
    list.push(hwnd);
    BOOL(1)
}

fn process_name(hwnd: HWND) -> String {
    let mut pid = 0u32;
    unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
    if pid == 0 {
        return String::new();
    }
    let Ok(handle) = (unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }) else {
        return String::new();
    };
    let mut buffer = vec![0u16; 1024];
    let mut size = buffer.len() as u32;
    let ok = unsafe {
        QueryFullProcessImageNameW(handle, PROCESS_NAME_WIN32, windows::core::PWSTR(buffer.as_mut_ptr()), &mut size)
    }
    .is_ok();
    let _ = unsafe { CloseHandle(handle) };
    if !ok {
        return String::new();
    }
    let full = String::from_utf16_lossy(&buffer[..size as usize]);
    Path::new(&full)
        .file_stem()
        .map(|s| s.to_string_lossy().to_lowercase())
        .unwrap_or_default()
}

fn window_title(hwnd: HWND) -> String {
    let length = unsafe { GetWindowTextLengthW(hwnd) };
    if length <= 0 {
        return String::new();
    }
    let mut buffer = vec![0u16; length as usize + 1];
    let written = unsafe { GetWindowTextW(hwnd, &mut buffer) };
    String::from_utf16_lossy(&buffer[..written.max(0) as usize])
}

fn dwm_cloaked(hwnd: HWND) -> bool {
    let mut value = 0u32;
    // Fail-closed: lässt sich der Status nicht lesen, gilt das Fenster als verdeckt.
    match unsafe {
        DwmGetWindowAttribute(
            hwnd,
            DWMWA_CLOAKED,
            &mut value as *mut u32 as *mut core::ffi::c_void,
            std::mem::size_of::<u32>() as u32,
        )
    } {
        Ok(()) => value != 0,
        Err(_) => true,
    }
}

fn extended_bounds(hwnd: HWND) -> Rect {
    let mut rect = RECT::default();
    let ok = unsafe {
        DwmGetWindowAttribute(
            hwnd,
            DWMWA_EXTENDED_FRAME_BOUNDS,
            &mut rect as *mut RECT as *mut core::ffi::c_void,
            std::mem::size_of::<RECT>() as u32,
        )
    }
    .is_ok();
    if ok {
        Rect { l: rect.left, t: rect.top, r: rect.right, b: rect.bottom }
    } else {
        Rect { l: 0, t: 0, r: 0, b: 0 }
    }
}

pub fn list_windows() -> Result<Vec<RawWindow>, String> {
    let mut handles: Vec<HWND> = Vec::new();
    unsafe { EnumWindows(Some(collect_window), LPARAM(&mut handles as *mut Vec<HWND> as isize)) }
        .map_err(|e| format!("EnumWindows fehlgeschlagen: {e}"))?;
    Ok(handles
        .into_iter()
        .map(|hwnd| {
            let visible = unsafe { IsWindowVisible(hwnd) }.as_bool();
            RawWindow {
                hwnd: hwnd.0 as isize as i64,
                process: process_name(hwnd),
                title: if visible { window_title(hwnd) } else { String::new() },
                rect: extended_bounds(hwnd),
                visible,
                minimized: unsafe { IsIconic(hwnd) }.as_bool(),
                cloaked: dwm_cloaked(hwnd),
            }
        })
        .collect())
}

unsafe extern "system" fn collect_monitor(monitor: HMONITOR, _hdc: HDC, _rect: *mut RECT, lparam: LPARAM) -> BOOL {
    let list = &mut *(lparam.0 as *mut Vec<MonitorInfo>);
    let mut info = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
    if GetMonitorInfoW(monitor, &mut info).as_bool() {
        let r = info.rcMonitor;
        list.push(MonitorInfo {
            handle: monitor.0 as isize as i64,
            rect: Rect { l: r.left, t: r.top, r: r.right, b: r.bottom },
        });
    }
    BOOL(1)
}

pub fn list_monitors() -> Result<Vec<MonitorInfo>, String> {
    let mut monitors: Vec<MonitorInfo> = Vec::new();
    let ok = unsafe {
        EnumDisplayMonitors(None, None, Some(collect_monitor), LPARAM(&mut monitors as *mut Vec<MonitorInfo> as isize))
    };
    if !ok.as_bool() {
        return Err("EnumDisplayMonitors fehlgeschlagen.".into());
    }
    Ok(monitors)
}

fn create_d3d_device() -> Result<(ID3D11Device, ID3D11DeviceContext), CaptureError> {
    let mut device: Option<ID3D11Device> = None;
    let mut context: Option<ID3D11DeviceContext> = None;
    unsafe {
        D3D11CreateDevice(
            None,
            D3D_DRIVER_TYPE_HARDWARE,
            HMODULE::default(),
            D3D11_CREATE_DEVICE_BGRA_SUPPORT,
            None,
            D3D11_SDK_VERSION,
            Some(&mut device),
            None,
            Some(&mut context),
        )
    }
    .map_err(map_err)?;
    match (device, context) {
        (Some(d), Some(c)) => Ok((d, c)),
        _ => Err(CaptureError::Runtime("D3D11-Gerät konnte nicht erstellt werden.".into())),
    }
}

fn best_effort_session(session: &GraphicsCaptureSession) {
    // Nur wenn die Eigenschaft existiert; Fehler sind bewusst folgenlos (ältere Builds).
    if ApiInformation::IsPropertyPresent(&HSTRING::from(PROPERTY_TYPE), &HSTRING::from("IsBorderRequired")).unwrap_or(false) {
        let _ = session.SetIsBorderRequired(false);
    }
    if ApiInformation::IsPropertyPresent(&HSTRING::from(PROPERTY_TYPE), &HSTRING::from("IsCursorCaptureEnabled")).unwrap_or(false) {
        let _ = session.SetIsCursorCaptureEnabled(false);
    }
}

fn read_frame(device: &ID3D11Device, context: &ID3D11DeviceContext, frame: &Direct3D11CaptureFrame) -> Result<Frame, CaptureError> {
    let surface = frame.Surface().map_err(map_err)?;
    let access: IDirect3DDxgiInterfaceAccess = surface.cast().map_err(map_err)?;
    let texture: ID3D11Texture2D = unsafe { access.GetInterface() }.map_err(map_err)?;

    let mut desc = D3D11_TEXTURE2D_DESC::default();
    unsafe { texture.GetDesc(&mut desc) };
    let (width, height) = (desc.Width as usize, desc.Height as usize);
    desc.Usage = D3D11_USAGE_STAGING;
    desc.BindFlags = 0;
    desc.CPUAccessFlags = D3D11_CPU_ACCESS_READ.0 as u32;
    desc.MiscFlags = 0;

    let mut staging: Option<ID3D11Texture2D> = None;
    unsafe { device.CreateTexture2D(&desc, None, Some(&mut staging)) }.map_err(map_err)?;
    let staging = staging.ok_or_else(|| CaptureError::Runtime("Staging-Textur fehlt.".into()))?;
    unsafe { context.CopyResource(&staging, &texture) };

    let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
    unsafe { context.Map(&staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped)) }.map_err(map_err)?;
    let mut bgra = vec![0u8; width * height * 4];
    let source = mapped.pData as *const u8;
    let pitch = mapped.RowPitch as usize;
    for row in 0..height {
        unsafe {
            std::ptr::copy_nonoverlapping(source.add(row * pitch), bgra.as_mut_ptr().add(row * width * 4), width * 4);
        }
    }
    unsafe { context.Unmap(&staging, 0) };
    Ok(Frame { width: width as u32, height: height as u32, bgra })
}

/// Erfasst genau ein Item über WGC. Wartet höchstens `timeout` auf einen Frame.
fn capture_item(item: &GraphicsCaptureItem, timeout: Duration) -> Result<Frame, CaptureError> {
    let (device, context) = create_d3d_device()?;
    let dxgi: IDXGIDevice = device.cast().map_err(map_err)?;
    let inspectable = unsafe { CreateDirect3D11DeviceFromDXGIDevice(&dxgi) }.map_err(map_err)?;
    let d3d_device: IDirect3DDevice = inspectable.cast().map_err(map_err)?;

    let size = item.Size().map_err(map_err)?;
    let pool = Direct3D11CaptureFramePool::CreateFreeThreaded(&d3d_device, DirectXPixelFormat::B8G8R8A8UIntNormalized, 1, size)
        .map_err(map_err)?;
    let session = pool.CreateCaptureSession(item).map_err(map_err)?;
    best_effort_session(&session);

    let (sender, receiver) = mpsc::channel::<()>();
    let handler = TypedEventHandler::new(move |_, _| {
        let _ = sender.send(());
        Ok(())
    });
    let token = pool.FrameArrived(&handler).map_err(map_err)?;
    session.StartCapture().map_err(map_err)?;

    let deadline = Instant::now() + timeout;
    let result = loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() || receiver.recv_timeout(remaining).is_err() {
            break Err(CaptureError::NoFrame);
        }
        if let Ok(frame) = pool.TryGetNextFrame() {
            let read = read_frame(&device, &context, &frame);
            let _ = frame.Close();
            break read;
        }
    };

    let _ = pool.RemoveFrameArrived(token);
    let _ = session.Close();
    let _ = pool.Close();
    result
}

pub fn capture_window(hwnd: i64, timeout: Duration) -> Result<Frame, CaptureError> {
    ensure_ro_init();
    let handle = HWND(hwnd as isize as *mut core::ffi::c_void);
    if unsafe { IsIconic(handle) }.as_bool() {
        return Err(CaptureError::Minimized);
    }
    let interop: IGraphicsCaptureItemInterop =
        windows::core::factory::<GraphicsCaptureItem, IGraphicsCaptureItemInterop>().map_err(map_err)?;
    let item: GraphicsCaptureItem = unsafe { interop.CreateForWindow(handle) }.map_err(map_err)?;
    capture_item(&item, timeout)
}

pub fn capture_monitor(handle: i64, timeout: Duration) -> Result<Frame, CaptureError> {
    ensure_ro_init();
    let monitor = HMONITOR(handle as isize as *mut core::ffi::c_void);
    let interop: IGraphicsCaptureItemInterop =
        windows::core::factory::<GraphicsCaptureItem, IGraphicsCaptureItemInterop>().map_err(map_err)?;
    let item: GraphicsCaptureItem = unsafe { interop.CreateForMonitor(monitor) }.map_err(map_err)?;
    capture_item(&item, timeout)
}
