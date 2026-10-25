//! Explicit placement of owned benchmark windows, before first visibility.
use std::io;
#[derive(Clone, Copy, Debug, serde::Serialize)]
pub(crate) struct Monitor {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
    pub primary: bool,
}
#[cfg(windows)]
pub(crate) fn inventory() -> io::Result<Vec<Monitor>> {
    use windows_sys::Win32::{
        Foundation::{LPARAM, RECT},
        Graphics::Gdi::*,
    };
    unsafe extern "system" fn collect(
        monitor: HMONITOR,
        _: HDC,
        _: *mut RECT,
        data: LPARAM,
    ) -> i32 {
        let monitors = unsafe { &mut *(data as *mut Vec<Monitor>) };
        let mut info = MONITORINFO {
            cbSize: std::mem::size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        if unsafe { GetMonitorInfoW(monitor, &mut info) } != 0 && monitors.len() < 16 {
            monitors.push(Monitor {
                x: info.rcWork.left,
                y: info.rcWork.top,
                width: info.rcWork.right - info.rcWork.left,
                height: info.rcWork.bottom - info.rcWork.top,
                primary: info.dwFlags & 1 != 0,
            });
        }
        1
    }
    let mut monitors = Vec::new();
    // SAFETY: enumeration is synchronous; the callback's uniquely owned Vec
    // remains valid until it returns. This only reads display geometry.
    if unsafe {
        EnumDisplayMonitors(
            std::ptr::null_mut(),
            std::ptr::null(),
            Some(collect),
            (&mut monitors as *mut Vec<Monitor>) as isize,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok(monitors)
}
#[cfg(not(windows))]
pub(crate) fn inventory() -> io::Result<Vec<Monitor>> {
    Err(io::Error::other(
        "secondary display placement requires Windows",
    ))
}

pub(crate) fn secondary() -> io::Result<Monitor> {
    inventory()?
        .into_iter()
        .find(|monitor| !monitor.primary && monitor.width >= 640 && monitor.height >= 480)
        .ok_or_else(|| {
            io::Error::other("No usable secondary display; benchmark window was not opened")
        })
}
#[cfg(windows)]
pub(crate) fn place(context: &eframe::CreationContext<'_>, monitor: Monitor) -> io::Result<()> {
    use raw_window_handle::HasWindowHandle;
    use windows_sys::Win32::UI::WindowsAndMessaging::*;
    let raw_window_handle::RawWindowHandle::Win32(handle) =
        context.window_handle().map_err(io::Error::other)?.as_raw()
    else {
        return Err(io::Error::other("Expected Win32 benchmark window"));
    };
    let width = 1200.min(monitor.width - 64);
    let height = 800.min(monitor.height - 64);
    // SAFETY: CreationContext owns this HWND. It is still hidden, and coordinates
    // are physical pixels in the chosen non-primary monitor's work rectangle.
    if unsafe {
        SetWindowPos(
            handle.hwnd.get() as _,
            std::ptr::null_mut(),
            monitor.x + (monitor.width - width) / 2,
            monitor.y + (monitor.height - height) / 2,
            width,
            height,
            SWP_NOACTIVATE | SWP_NOZORDER,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    context
        .egui_ctx
        .send_viewport_cmd(egui::ViewportCommand::Visible(true));
    Ok(())
}
#[cfg(not(windows))]
pub(crate) fn place(_: &eframe::CreationContext<'_>, _: Monitor) -> io::Result<()> {
    Err(io::Error::other("unsupported display placement"))
}
