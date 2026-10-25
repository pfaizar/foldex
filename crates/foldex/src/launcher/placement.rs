//! Keep the launcher centered as its content, DPI or display work area changes.
//! The subclass belongs only to our popup HWND and adds no polling or input hook.

fn centered(area: [i32; 4], requested: [i32; 2]) -> [i32; 4] {
    let [left, top, right, bottom] = area;
    let width = requested[0].clamp(1, (right - left - 32).max(1));
    let height = requested[1].clamp(1, (bottom - top - 48).max(1));
    [
        left + (right - left - width) / 2,
        top + (bottom - top - height) / 2,
        width,
        height,
    ]
}

#[cfg(windows)]
mod win {
    use super::centered;
    use crate::performance::monitor::Monitor;
    use windows_sys::Win32::{
        Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM},
        Graphics::Gdi::*,
        UI::{
            Shell::{DefSubclassProc, RemoveWindowSubclass, SetWindowSubclass},
            WindowsAndMessaging::*,
        },
    };

    const ID: usize = 0x464c504c;
    const PROPERTY: windows_sys::core::PCWSTR = windows_sys::core::w!("Foldex.Launcher.Placement");
    struct State {
        monitor: HMONITOR,
        override_monitor: Option<Monitor>,
    }
    fn monitor_at(point: Option<POINT>, previous: isize) -> HMONITOR {
        if let Some(point) = point {
            let monitor = unsafe { MonitorFromPoint(point, MONITOR_DEFAULTTONULL) };
            if !monitor.is_null() {
                return monitor;
            }
        }
        unsafe { MonitorFromWindow(previous as HWND, MONITOR_DEFAULTTONEAREST) }
    }

    fn activation_monitor(previous: isize) -> HMONITOR {
        let mut point = POINT::default();
        let available = unsafe { GetCursorPos(&mut point) } != 0;
        monitor_at(available.then_some(point), previous)
    }
    impl State {
        fn area(&self, hwnd: HWND) -> Option<[i32; 4]> {
            if let Some(monitor) = self.override_monitor {
                return Some([
                    monitor.x,
                    monitor.y,
                    monitor.x + monitor.width,
                    monitor.y + monitor.height,
                ]);
            }
            let mut info = MONITORINFO {
                cbSize: std::mem::size_of::<MONITORINFO>() as u32,
                ..Default::default()
            };
            // Retain the monitor chosen on activation. A disconnected display
            // falls back to the nearest surviving display for this popup.
            if unsafe { GetMonitorInfoW(self.monitor, &mut info) } == 0
                && unsafe {
                    GetMonitorInfoW(MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST), &mut info)
                } == 0
            {
                return None;
            }
            let r = info.rcWork;
            Some([r.left, r.top, r.right, r.bottom])
        }
    }

    pub(crate) fn install(
        hwnd: HWND,
        previous: isize,
        override_monitor: Option<Monitor>,
        requested: [i32; 2],
    ) -> Option<[i32; 4]> {
        // Called by the owning UI thread before showing the launcher. Its
        // state is released by WM_NCDESTROY, including viewport recreation.
        let state = State {
            monitor: if override_monitor.is_some() {
                std::ptr::null_mut()
            } else {
                activation_monitor(previous)
            },
            override_monitor,
        };
        let rect = centered(state.area(hwnd)?, requested);
        let data = unsafe { GetPropW(hwnd, PROPERTY) };
        if !data.is_null() {
            unsafe {
                *(data as *mut State) = state;
            }
        } else {
            let state = Box::into_raw(Box::new(state));
            if unsafe { SetWindowSubclass(hwnd, Some(callback), ID, state as usize) } == 0 {
                unsafe {
                    drop(Box::from_raw(state));
                }
                return None;
            }
            if unsafe { SetPropW(hwnd, PROPERTY, state.cast()) } == 0 {
                unsafe {
                    RemoveWindowSubclass(hwnd, Some(callback), ID);
                    drop(Box::from_raw(state));
                }
                return None;
            }
        }
        Some(rect)
    }

    unsafe extern "system" fn callback(
        hwnd: HWND,
        msg: u32,
        wp: WPARAM,
        lp: LPARAM,
        _id: usize,
        data: usize,
    ) -> LRESULT {
        if msg == WM_NCDESTROY {
            unsafe {
                RemoveWindowSubclass(hwnd, Some(callback), ID);
                RemovePropW(hwnd, PROPERTY);
                drop(Box::from_raw(data as *mut State));
                return DefSubclassProc(hwnd, msg, wp, lp);
            }
        }
        if msg == WM_NCHITTEST {
            return HTCLIENT as LRESULT;
        }
        if msg == WM_SYSCOMMAND
            && matches!(
                (wp as u32) & 0xfff0,
                SC_MOVE | SC_SIZE | SC_MAXIMIZE | SC_MINIMIZE | SC_RESTORE
            )
        {
            return 0;
        }
        if msg == WM_WINDOWPOSCHANGING && lp != 0 {
            let state = unsafe { &*(data as *const State) };
            let position = unsafe { &mut *(lp as *mut WINDOWPOS) };
            let mut current = RECT::default();
            if let Some(area) = state.area(hwnd)
                && unsafe { GetWindowRect(hwnd, &mut current) } != 0
            {
                let requested = if position.flags & SWP_NOSIZE != 0 {
                    [current.right - current.left, current.bottom - current.top]
                } else {
                    [position.cx, position.cy]
                };
                let [x, y, width, height] = centered(area, requested);
                position.x = x;
                position.y = y;
                position.cx = width;
                position.cy = height;
                if x != current.left || y != current.top {
                    position.flags &= !SWP_NOMOVE;
                }
                if width != current.right - current.left || height != current.bottom - current.top {
                    position.flags &= !SWP_NOSIZE;
                }
                // Preserve show/hide and activation flags. The regular WM_SIZE
                // notification still reaches winit when content adapts.
                return 0;
            }
        }
        let result = unsafe { DefSubclassProc(hwnd, msg, wp, lp) };
        if matches!(msg, WM_DISPLAYCHANGE | WM_SETTINGCHANGE) {
            unsafe {
                SetWindowPos(
                    hwnd,
                    std::ptr::null_mut(),
                    0,
                    0,
                    0,
                    0,
                    SWP_NOSIZE | SWP_NOACTIVATE | SWP_NOZORDER | SWP_NOOWNERZORDER,
                );
            }
        }
        result
    }

    pub(crate) fn locked(hwnd: HWND) -> bool {
        if unsafe { GetPropW(hwnd, PROPERTY) }.is_null() {
            return false;
        }
        let before = super::super::launcher_bounds_from_handle(hwnd);
        let Some(rect) = before else {
            return false;
        };
        // Owned native regression probe; no desktop keyboard/mouse events.
        unsafe {
            for command in [SC_MOVE, SC_SIZE, SC_MAXIMIZE, SC_MINIMIZE, SC_RESTORE] {
                SendMessageW(hwnd, WM_SYSCOMMAND, command as WPARAM, 0);
            }
            SetWindowPos(
                hwnd,
                std::ptr::null_mut(),
                rect[0] + 77,
                rect[1] - 43,
                0,
                0,
                SWP_NOSIZE | SWP_NOACTIVATE | SWP_NOZORDER | SWP_NOOWNERZORDER,
            );
        }
        before == super::super::launcher_bounds_from_handle(hwnd)
            && unsafe { GetWindowLongPtrW(hwnd, GWL_STYLE) as u32 }
                & (WS_THICKFRAME | WS_MAXIMIZEBOX | WS_MINIMIZEBOX)
                == 0
    }

    /// Exercise real monitor selection without moving the desktop cursor or
    /// creating a window on any tested monitor.
    pub(crate) fn selection_follows_point(previous: isize, monitors: &[Monitor]) -> bool {
        !monitors.is_empty()
            && monitors.iter().all(|monitor| {
                let state = State {
                    monitor: monitor_at(
                        Some(POINT {
                            x: monitor.x + monitor.width / 2,
                            y: monitor.y + monitor.height / 2,
                        }),
                        previous,
                    ),
                    override_monitor: None,
                };
                state.area(previous as HWND)
                    == Some([
                        monitor.x,
                        monitor.y,
                        monitor.x + monitor.width,
                        monitor.y + monitor.height,
                    ])
            })
            && monitor_at(None, previous)
                == unsafe { MonitorFromWindow(previous as HWND, MONITOR_DEFAULTTONEAREST) }
    }
}

#[cfg(windows)]
pub(super) use win::{install, locked, selection_follows_point};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn popup_stays_centered_and_inside_small_or_offset_work_areas() {
        for area in [
            [0, 0, 1920, 1032],
            [-1920, -200, 0, 832],
            [1920, 0, 5760, 2112],
            [10, 20, 490, 340],
            [0, 0, 40, 40],
        ] {
            for size in [[640, 420], [1280, 840], [380, 180], [1, 1]] {
                let [x, y, w, h] = centered(area, size);
                assert!(w > 0 && h > 0);
                assert!(x >= area[0] && y >= area[1]);
                assert!(x + w <= area[2] && y + h <= area[3]);
                assert!((2 * x + w - area[0] - area[2]).abs() <= 1);
                assert!((2 * y + h - area[1] - area[3]).abs() <= 1);
            }
        }
    }
}
