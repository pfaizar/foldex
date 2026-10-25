//! Win32 non-client behavior for the integrated title strip.
//! Windows owns edge sizing, caption actions and the Snap Layout flyout.
#[cfg(windows)]
mod imp {
    use std::sync::{
        Arc,
        atomic::{AtomicBool, AtomicI32, AtomicU32, Ordering},
    };
    use windows_sys::Win32::{
        Foundation::*,
        Graphics::{Dwm::*, Gdi::*},
        UI::{
            HiDpi::*,
            Input::KeyboardAndMouse::*,
            Shell::{DefSubclassProc, RemoveWindowSubclass, SetWindowSubclass},
            WindowsAndMessaging::*,
        },
    };
    const ID: usize = 0x46494c4544;
    struct State {
        ctx: egui::Context,
        installed: AtomicBool,
        hover: AtomicI32,
        pressed: AtomicI32,
        color: AtomicU32,
        border: AtomicU32,
        fullscreen: AtomicBool,
        sizing: AtomicBool,
    }
    pub struct NativeFrame {
        hwnd: HWND,
        state: Arc<State>,
    }
    impl NativeFrame {
        pub fn new(owner: isize, ctx: &egui::Context) -> Option<Self> {
            if owner == 0 {
                return None;
            }
            let hwnd = owner as HWND;
            let state = Arc::new(State {
                ctx: ctx.clone(),
                installed: AtomicBool::new(true),
                hover: AtomicI32::new(0),
                pressed: AtomicI32::new(0),
                color: AtomicU32::new(u32::MAX),
                border: AtomicU32::new(u32::MAX),
                fullscreen: AtomicBool::new(false),
                sizing: AtomicBool::new(false),
            });
            let raw = Arc::into_raw(state.clone());
            unsafe {
                if SetWindowSubclass(hwnd, Some(callback), ID, raw as usize) == 0 {
                    drop(Arc::from_raw(raw));
                    return None;
                }
                let style = GetWindowLongPtrW(hwnd, GWL_STYLE) as u32
                    | WS_THICKFRAME
                    | WS_CAPTION
                    | WS_SYSMENU
                    | WS_MINIMIZEBOX
                    | WS_MAXIMIZEBOX;
                SetWindowLongPtrW(hwnd, GWL_STYLE, style as isize);
                let dark = 1_i32;
                DwmSetWindowAttribute(
                    hwnd,
                    DWMWA_USE_IMMERSIVE_DARK_MODE as u32,
                    (&dark as *const i32).cast(),
                    4,
                );
                let border = 53_u32 | 58 << 8 | 60 << 16;
                DwmSetWindowAttribute(
                    hwnd,
                    DWMWA_BORDER_COLOR as u32,
                    (&border as *const u32).cast(),
                    4,
                );
                let corner = DWMWCP_ROUND;
                DwmSetWindowAttribute(
                    hwnd,
                    DWMWA_WINDOW_CORNER_PREFERENCE as u32,
                    (&corner as *const i32).cast(),
                    4,
                );
                SetWindowPos(
                    hwnd,
                    std::ptr::null_mut(),
                    0,
                    0,
                    0,
                    0,
                    SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_NOZORDER | SWP_FRAMECHANGED,
                );
            }
            Some(Self { hwnd, state })
        }
        pub fn hover(&self, n: usize) -> bool {
            self.state.hover.load(Ordering::Relaxed)
                == match n {
                    0 => HTMINBUTTON,
                    1 => HTMAXBUTTON,
                    _ => HTCLOSE,
                } as i32
        }
        pub fn is_sizing(&self) -> bool {
            self.state.sizing.load(Ordering::Relaxed)
        }
        pub fn benchmark_rect(&self) -> Option<[i32; 4]> {
            let mut rect = RECT::default();
            (unsafe { GetWindowRect(self.hwnd, &mut rect) } != 0).then_some([
                rect.left,
                rect.top,
                rect.right - rect.left,
                rect.bottom - rect.top,
            ])
        }
        /// Isolated benchmark only: resizes this application's own window.
        pub fn benchmark_resize(&self, rect: [i32; 4], active: bool) {
            self.state.sizing.store(active, Ordering::Relaxed);
            unsafe {
                SetWindowPos(
                    self.hwnd,
                    std::ptr::null_mut(),
                    rect[0],
                    rect[1],
                    rect[2],
                    rect[3],
                    SWP_NOACTIVATE | SWP_NOZORDER,
                );
            }
            self.state.ctx.request_repaint();
        }
        pub fn update(&self, bg: [u8; 3], border: [u8; 3], fullscreen: bool) {
            if self.state.fullscreen.swap(fullscreen, Ordering::Relaxed) != fullscreen {
                unsafe {
                    SetWindowPos(
                        self.hwnd,
                        std::ptr::null_mut(),
                        0,
                        0,
                        0,
                        0,
                        SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_NOZORDER | SWP_FRAMECHANGED,
                    );
                }
            }
            let color = bg[0] as u32 | (bg[1] as u32) << 8 | (bg[2] as u32) << 16;
            if self.state.color.swap(color, Ordering::Relaxed) != color {
                #[cfg(feature = "renderer-wgpu")]
                wgpu::hal::dx12::set_window_background_color(bg);
                let dark = i32::from(bg.iter().map(|v| *v as u32).sum::<u32>() < 384);
                unsafe {
                    DwmSetWindowAttribute(
                        self.hwnd,
                        DWMWA_USE_IMMERSIVE_DARK_MODE as u32,
                        (&dark as *const i32).cast(),
                        4,
                    );
                    DwmSetWindowAttribute(
                        self.hwnd,
                        DWMWA_CAPTION_COLOR as u32,
                        (&color as *const u32).cast(),
                        4,
                    );
                }
            }
            let color = border[0] as u32 | (border[1] as u32) << 8 | (border[2] as u32) << 16;
            if self.state.border.swap(color, Ordering::Relaxed) != color {
                unsafe {
                    DwmSetWindowAttribute(
                        self.hwnd,
                        DWMWA_BORDER_COLOR as u32,
                        (&color as *const u32).cast(),
                        4,
                    );
                }
            }
        }
    }
    impl Drop for NativeFrame {
        fn drop(&mut self) {
            #[cfg(feature = "renderer-wgpu")]
            eframe::forget_windows_early_resize(self.hwnd as isize);
            unsafe {
                if self.state.installed.swap(false, Ordering::Relaxed) {
                    RemoveWindowSubclass(self.hwnd, Some(callback), ID);
                    drop(Arc::from_raw(Arc::as_ptr(&self.state)));
                }
            }
        }
    }

    #[cfg(feature = "renderer-wgpu")]
    fn paint_proposed_resize(hwnd: HWND, rect: [i32; 4]) {
        let Some(width) = rect[2].checked_sub(rect[0]).filter(|width| *width > 0) else {
            return;
        };
        let Some(height) = rect[3].checked_sub(rect[1]).filter(|height| *height > 0) else {
            return;
        };
        let mut current: RECT = unsafe { std::mem::zeroed() };
        if unsafe { GetClientRect(hwnd, &mut current) } == 0
            || (current.right - current.left == width && current.bottom - current.top == height)
        {
            return;
        }
        let expected_size = [width as u32, height as u32];
        eframe::with_windows_early_resize(hwnd as isize, rect, || {
            let before = wgpu::hal::dx12::last_composition_window_present();
            // winit paints synchronously when its application handler is idle;
            // it defers instead when already borrowed. The scoped hint then
            // expires and WM_SIZE remains responsible for the real geometry.
            unsafe { SendMessageW(hwnd, WM_PAINT, 0, 0) };
            let after = wgpu::hal::dx12::last_composition_window_present();
            after != before
                && after
                    .is_some_and(|(owner, size, _)| owner == hwnd as isize && size == expected_size)
        });
    }

    unsafe extern "system" fn callback(
        hwnd: HWND,
        msg: u32,
        wp: WPARAM,
        lp: LPARAM,
        _id: usize,
        data: usize,
    ) -> LRESULT {
        unsafe {
            Arc::increment_strong_count(data as *const State);
        }
        let keep = unsafe { Arc::from_raw(data as *const State) };
        let state = &*keep;
        if msg == WM_NCDESTROY {
            #[cfg(feature = "renderer-wgpu")]
            eframe::forget_windows_early_resize(hwnd as isize);
            let result = unsafe { DefSubclassProc(hwnd, msg, wp, lp) };
            if state.installed.swap(false, Ordering::Relaxed) {
                unsafe {
                    RemoveWindowSubclass(hwnd, Some(callback), ID);
                    drop(Arc::from_raw(data as *const State));
                }
            }
            return result;
        }
        if msg == WM_NCCALCSIZE {
            let fullscreen = state.fullscreen.load(Ordering::Relaxed);
            let maximized = unsafe { IsZoomed(hwnd) } != 0;
            if !fullscreen {
                // Preserve the native side/bottom frame; integrate only the title strip.
                // File Pilot's window procedure uses the same frame metrics (REA 0x1401b2e80).
                let dpi = unsafe { GetDpiForWindow(hwnd) }.max(96);
                let padding = unsafe { GetSystemMetricsForDpi(SM_CXPADDEDBORDER, dpi) };
                let x = unsafe { GetSystemMetricsForDpi(SM_CXSIZEFRAME, dpi) } + padding;
                let y = unsafe { GetSystemMetricsForDpi(SM_CYSIZEFRAME, dpi) } + padding;
                let rect = unsafe { &mut *(lp as *mut RECT) };
                rect.left += x;
                rect.right -= x;
                rect.bottom -= y;
                if maximized {
                    rect.top += y;
                }
            }
            #[cfg(feature = "renderer-wgpu")]
            if wp != 0
                && state.sizing.load(Ordering::Relaxed)
                && !fullscreen
                && !maximized
                && unsafe { IsWindowVisible(hwnd) } != 0
                && unsafe { IsIconic(hwnd) } == 0
                && unsafe { GetWindowLongPtrW(hwnd, GWL_EXSTYLE) } as u32
                    & WS_EX_NOREDIRECTIONBITMAP
                    != 0
            {
                // Copy after calculating all native insets. The HWND still has
                // its old client rectangle until this callback returns.
                let rect = unsafe { *(lp as *const RECT) };
                paint_proposed_resize(hwnd, [rect.left, rect.top, rect.right, rect.bottom]);
            }
            // The integrated caption has no top non-client inset when
            // restored. Preserve Windows' client-pixel/position bookkeeping
            // for that frame shape; WVR_REDRAW discards it before the smaller
            // swapchain frame and the native window geometry are composited.
            // This is also the required result for the single-RECT form.
            return 0;
        }
        if msg == WM_ERASEBKGND {
            // Presentation owns the complete client surface. Do not mix a GDI
            // background fill with the DXGI flip-model frames: it can replace
            // rendered content with an intermediate blank frame. Transient
            // buffer/target size mismatches must be covered by presentation.
            return 1;
        }
        if msg == WM_NCHITTEST && !state.fullscreen.load(Ordering::Relaxed) {
            let screen_point = POINT {
                x: (lp as u32 & 0xffff) as i16 as i32,
                y: ((lp as u32 >> 16) & 0xffff) as i16 as i32,
            };
            let mut point = screen_point;
            let mut rect: RECT = unsafe { std::mem::zeroed() };
            unsafe {
                ScreenToClient(hwnd, &mut point);
                GetClientRect(hwnd, &mut rect);
            }
            let dpi = unsafe { GetDpiForWindow(hwnd) }.max(96);
            let scale = dpi as f32 / 96.0;
            let edge = unsafe {
                GetSystemMetricsForDpi(SM_CXSIZEFRAME, dpi)
                    + GetSystemMetricsForDpi(SM_CXPADDEDBORDER, dpi)
            }
            .max(4);
            if unsafe { IsZoomed(hwnd) } == 0 {
                let mut outer: RECT = unsafe { std::mem::zeroed() };
                unsafe {
                    GetWindowRect(hwnd, &mut outer);
                }
                // Include the visible client boundary in the native sizing surface.
                let inner = (6.0 * scale).round() as i32;
                let left = screen_point.x < outer.left + edge || point.x < inner;
                let right = screen_point.x >= outer.right - edge || point.x >= rect.right - inner;
                let top = screen_point.y < outer.top + edge || point.y < inner;
                let bottom =
                    screen_point.y >= outer.bottom - edge || point.y >= rect.bottom - inner;
                let hit = match (left, right, top, bottom) {
                    (true, _, true, _) => HTTOPLEFT,
                    (_, true, true, _) => HTTOPRIGHT,
                    (true, _, _, true) => HTBOTTOMLEFT,
                    (_, true, _, true) => HTBOTTOMRIGHT,
                    (true, _, _, _) => HTLEFT,
                    (_, true, _, _) => HTRIGHT,
                    (_, _, true, _) => HTTOP,
                    (_, _, _, true) => HTBOTTOM,
                    _ => HTCLIENT,
                };
                if hit != HTCLIENT {
                    return hit as LRESULT;
                }
            }
            let mut result = 0;
            if unsafe { DwmDefWindowProc(hwnd, msg, wp, lp, &mut result) } != 0 {
                return result;
            }
            if point.y < (32.0 * scale) as i32 {
                let width = (46.0 * scale) as i32;
                if point.x >= rect.right - width {
                    return HTCLOSE as LRESULT;
                }
                if point.x >= rect.right - width * 2 {
                    return HTMAXBUTTON as LRESULT;
                }
                if point.x >= rect.right - width * 3 {
                    return HTMINBUTTON as LRESULT;
                }
            }
            return HTCLIENT as LRESULT;
        }
        if msg == WM_NCMOUSEMOVE {
            let hover = wp as i32;
            if state.hover.swap(hover, Ordering::Relaxed) != hover {
                state.ctx.request_repaint();
            }
            let mut track = TRACKMOUSEEVENT {
                cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                dwFlags: TME_LEAVE | TME_NONCLIENT,
                hwndTrack: hwnd,
                dwHoverTime: 0,
            };
            unsafe {
                TrackMouseEvent(&mut track);
            }
        } else if msg == WM_NCMOUSELEAVE {
            state.hover.store(0, Ordering::Relaxed);
            state.ctx.request_repaint();
        }
        if msg == WM_ENTERSIZEMOVE {
            state.sizing.store(true, Ordering::Relaxed);
        }
        if msg == WM_EXITSIZEMOVE {
            state.sizing.store(false, Ordering::Relaxed);
            // End the direct-layout mode even when the final operation was a
            // move. Resize paints in WM_NCCALCSIZE with a WM_SIZE fallback;
            // requesting here or in WM_SIZING queues extra passes.
            state.ctx.request_repaint();
        }
        if msg == WM_NCLBUTTONDOWN && matches!(wp as u32, HTMINBUTTON | HTMAXBUTTON | HTCLOSE) {
            // The integrated strip has no Win32 caption button bounds. Capture a
            // cancellable press, then let Windows execute its native system command.
            state.pressed.store(wp as i32, Ordering::Relaxed);
            unsafe {
                SetCapture(hwnd);
            }
            state.ctx.request_repaint();
            return 0;
        }
        if msg == WM_CAPTURECHANGED || msg == WM_CANCELMODE {
            if state.pressed.swap(0, Ordering::Relaxed) != 0 {
                state.ctx.request_repaint();
            }
            if msg == WM_CANCELMODE && unsafe { GetCapture() } == hwnd {
                unsafe {
                    ReleaseCapture();
                }
            }
        }
        if state.pressed.load(Ordering::Relaxed) != 0
            && matches!(msg, WM_MOUSEMOVE | WM_LBUTTONUP | WM_NCLBUTTONUP)
        {
            let mut point: POINT = unsafe { std::mem::zeroed() };
            unsafe {
                GetCursorPos(&mut point);
                ScreenToClient(hwnd, &mut point);
            }
            let mut rect: RECT = unsafe { std::mem::zeroed() };
            unsafe {
                GetClientRect(hwnd, &mut rect);
            }
            let scale = unsafe { GetDpiForWindow(hwnd) }.max(96) as f32 / 96.0;
            let width = (46.0 * scale) as i32;
            let hit = if point.y >= 0 && point.y < (32.0 * scale) as i32 && point.x < rect.right {
                if point.x >= rect.right - width {
                    HTCLOSE
                } else if point.x >= rect.right - width * 2 {
                    HTMAXBUTTON
                } else if point.x >= rect.right - width * 3 {
                    HTMINBUTTON
                } else {
                    HTCLIENT
                }
            } else {
                HTCLIENT
            };
            state.hover.store(hit as i32, Ordering::Relaxed);
            state.ctx.request_repaint();
            if msg != WM_MOUSEMOVE {
                let pressed = state.pressed.swap(0, Ordering::Relaxed);
                unsafe {
                    ReleaseCapture();
                }
                if pressed == hit as i32 {
                    let command = if hit == HTCLOSE {
                        SC_CLOSE
                    } else if hit == HTMINBUTTON {
                        SC_MINIMIZE
                    } else if unsafe { IsZoomed(hwnd) } != 0 {
                        SC_RESTORE
                    } else {
                        SC_MAXIMIZE
                    };
                    unsafe {
                        PostMessageW(hwnd, WM_SYSCOMMAND, command as WPARAM, 0);
                    }
                }
            }
            return 0;
        }
        // Let DWM handle caption hover and native Snap Layouts before winit.
        if matches!(
            msg,
            WM_NCMOUSEMOVE
                | WM_NCMOUSELEAVE
                | WM_NCLBUTTONDOWN
                | WM_NCLBUTTONUP
                | WM_NCLBUTTONDBLCLK
                | WM_NCRBUTTONDOWN
                | WM_NCRBUTTONUP
                | WM_NCRBUTTONDBLCLK
        ) {
            let mut result = 0;
            if unsafe { DwmDefWindowProc(hwnd, msg, wp, lp, &mut result) } != 0 {
                return result;
            }
        }
        unsafe { DefSubclassProc(hwnd, msg, wp, lp) }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        fn state(fullscreen: bool) -> Arc<State> {
            Arc::new(State {
                ctx: egui::Context::default(),
                installed: AtomicBool::new(false),
                hover: AtomicI32::new(0),
                pressed: AtomicI32::new(0),
                color: AtomicU32::new(u32::MAX),
                border: AtomicU32::new(u32::MAX),
                fullscreen: AtomicBool::new(fullscreen),
                sizing: AtomicBool::new(false),
            })
        }

        #[test]
        fn native_client_resize_preserves_pixels_with_integrated_top_frame() {
            let state = state(false);
            let mut params = NCCALCSIZE_PARAMS::default();
            params.rgrc = [
                RECT {
                    left: 300,
                    top: 100,
                    right: 1400,
                    bottom: 900,
                },
                RECT {
                    left: 100,
                    top: 100,
                    right: 1400,
                    bottom: 900,
                },
                RECT {
                    left: 108,
                    top: 100,
                    right: 1392,
                    bottom: 892,
                },
            ];
            let old_client = params.rgrc[2];
            // Only the handled NCCALCSIZE branch runs. A null HWND represents
            // a restored window for these read-only frame-metric queries.
            let result = unsafe {
                callback(
                    std::ptr::null_mut(),
                    WM_NCCALCSIZE,
                    1,
                    (&mut params as *mut NCCALCSIZE_PARAMS) as LPARAM,
                    ID,
                    Arc::as_ptr(&state) as usize,
                )
            };
            assert_eq!(result, 0);
            assert_eq!(params.rgrc[0].top, 100);
            assert!(params.rgrc[0].left > 300);
            assert!(params.rgrc[0].right < 1400);
            assert!(params.rgrc[0].bottom < 900);
            assert_eq!(params.rgrc[2].left, old_client.left);
            assert_eq!(params.rgrc[2].right, old_client.right);
            assert_eq!(Arc::strong_count(&state), 1);
        }

        #[test]
        fn native_client_calculation_handles_single_rect_and_fullscreen() {
            let state = state(true);
            let expected = RECT {
                left: -1200,
                top: -400,
                right: -100,
                bottom: 400,
            };
            let mut rect = expected;
            for valid_rects in [0, 1] {
                let mut params = NCCALCSIZE_PARAMS::default();
                params.rgrc[0] = expected;
                let lp = if valid_rects == 0 {
                    (&mut rect as *mut RECT) as LPARAM
                } else {
                    (&mut params as *mut NCCALCSIZE_PARAMS) as LPARAM
                };
                let result = unsafe {
                    callback(
                        std::ptr::null_mut(),
                        WM_NCCALCSIZE,
                        valid_rects,
                        lp,
                        ID,
                        Arc::as_ptr(&state) as usize,
                    )
                };
                assert_eq!(result, 0);
                let actual = if valid_rects == 0 {
                    rect
                } else {
                    params.rgrc[0]
                };
                assert_eq!(
                    (actual.left, actual.top, actual.right, actual.bottom),
                    (expected.left, expected.top, expected.right, expected.bottom)
                );
            }
            assert_eq!(Arc::strong_count(&state), 1);
        }
    }
}
#[cfg(windows)]
pub use imp::NativeFrame;
#[cfg(not(windows))]
pub struct NativeFrame;
#[cfg(not(windows))]
impl NativeFrame {
    pub fn new(_: isize, _: &egui::Context) -> Option<Self> {
        None
    }
    pub fn hover(&self, _: usize) -> bool {
        false
    }
    pub fn is_sizing(&self) -> bool {
        false
    }
    pub fn benchmark_rect(&self) -> Option<[i32; 4]> {
        None
    }
    pub fn benchmark_resize(&self, _: [i32; 4], _: bool) {}
    pub fn update(&self, _: [u8; 3], _: [u8; 3], _: bool) {}
}
