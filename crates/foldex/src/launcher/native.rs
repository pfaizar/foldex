//! Native tray, global hotkey and clipboard notifications. No disk/provider work
//! runs in the window procedure. The hidden message thread sleeps when idle.
use std::collections::VecDeque;
#[path = "placement.rs"]
mod placement;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicIsize, AtomicU32, AtomicU64, Ordering},
    mpsc::SyncSender,
};
pub const TOGGLE: u32 = 1;
pub const EXPLORER: u32 = 2;
pub const SETTINGS: u32 = 4;
pub const EXIT: u32 = 8;
pub const STATUS: u32 = 16;
pub const ACTIVATE: u32 = 32;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct HotkeyFeedback {
    pub requested: String,
    pub active: Option<String>,
    pub error: Option<String>,
    pub pending: bool,
}

pub struct State {
    pub events: AtomicU32,
    pub previous: AtomicIsize,
    pub opened: AtomicBool,
    pub clipboard_version: AtomicU64,
    pub tray_ready: AtomicBool,
    pub status: Mutex<Option<String>>,
    pub activations: Mutex<VecDeque<super::instance::Activation>>,
    channel: Option<String>,
    hwnd: AtomicIsize,
    stopped: AtomicBool,
    enabled: AtomicBool,
    clipboard: AtomicBool,
    hotkey: Mutex<String>,
    hotkey_feedback: Mutex<HotkeyFeedback>,
    signal: SyncSender<()>,
    wake: Arc<dyn Fn() + Send + Sync>,
}
impl State {
    fn event(&self, event: u32) {
        self.events.fetch_or(event, Ordering::Release);
        (self.wake)();
    }
    pub fn window(&self) -> isize {
        self.hwnd.load(Ordering::Acquire)
    }
    pub fn remembers_clipboard(&self) -> bool {
        self.clipboard.load(Ordering::Acquire)
    }
}
pub struct Native {
    pub state: Arc<State>,
    thread: Option<std::thread::JoinHandle<()>>,
}
impl Native {
    pub fn new(
        enabled: bool,
        clipboard: bool,
        hotkey: String,
        wake: Arc<dyn Fn() + Send + Sync>,
        signal: SyncSender<()>,
        isolate: bool,
        channel: Option<String>,
    ) -> std::io::Result<Self> {
        let state = Arc::new(State {
            events: AtomicU32::new(0),
            previous: AtomicIsize::new(0),
            opened: AtomicBool::new(false),
            clipboard_version: AtomicU64::new(0),
            tray_ready: AtomicBool::new(false),
            status: Mutex::new(None),
            activations: Mutex::new(VecDeque::new()),
            channel,
            hwnd: AtomicIsize::new(0),
            stopped: AtomicBool::new(false),
            enabled: AtomicBool::new(enabled),
            clipboard: AtomicBool::new(clipboard),
            hotkey_feedback: Mutex::new(HotkeyFeedback {
                requested: hotkey.clone(),
                active: None,
                error: None,
                pending: enabled && !isolate,
            }),
            hotkey: Mutex::new(hotkey),
            signal,
            wake,
        });
        let thread = if !isolate {
            let thread = state.clone();
            Some(
                std::thread::Builder::new()
                    .name("foldex-tray".into())
                    .stack_size(256 * 1024)
                    .spawn(move || imp::run(thread))?,
            )
        } else {
            None
        };
        Ok(Self { state, thread })
    }
    pub fn configure(&self, enabled: bool, clipboard: bool, hotkey: &str) {
        self.state.enabled.store(enabled, Ordering::Release);
        self.state.clipboard.store(clipboard, Ordering::Release);
        *self.state.hotkey.lock().unwrap_or_else(|e| e.into_inner()) = hotkey.to_owned();
        {
            let mut feedback = self
                .state
                .hotkey_feedback
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            feedback.requested = hotkey.to_owned();
            feedback.error = None;
            feedback.pending = enabled && self.thread.is_some();
            if !enabled || self.thread.is_none() {
                feedback.active = None;
            }
        }
        imp::configure(self.state.hwnd.load(Ordering::Acquire));
        let _ = self.state.signal.try_send(());
    }
    pub fn hotkey_feedback(&self) -> HotkeyFeedback {
        self.state
            .hotkey_feedback
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }
    pub fn take_events(&self) -> u32 {
        self.state.events.swap(0, Ordering::AcqRel)
    }
    pub fn take_activations(&self) -> VecDeque<super::instance::Activation> {
        std::mem::take(
            &mut *self
                .state
                .activations
                .lock()
                .unwrap_or_else(|e| e.into_inner()),
        )
    }
    pub fn previous(&self) -> isize {
        self.state.previous.load(Ordering::Acquire)
    }
    pub fn set_opened(&self, opened: bool) {
        self.state.opened.store(opened, Ordering::Release);
    }
    pub fn capture_previous(&self) {
        let previous = imp::foreground();
        if previous != 0 && !self.state.opened.load(Ordering::Acquire) {
            self.state.previous.store(previous, Ordering::Release);
        }
    }
}

#[cfg(windows)]
fn owned_launcher(title: &str) -> Option<windows_sys::Win32::Foundation::HWND> {
    use windows_sys::Win32::UI::WindowsAndMessaging::{FindWindowW, GetWindowThreadProcessId};
    let title: Vec<_> = title.encode_utf16().chain(Some(0)).collect();
    let hwnd = unsafe { FindWindowW(std::ptr::null(), title.as_ptr()) };
    if hwnd.is_null() {
        return None;
    }
    let mut process = 0;
    unsafe {
        GetWindowThreadProcessId(hwnd, &mut process);
    }
    (process == std::process::id()).then_some(hwnd)
}

/// Hide on the owning UI thread before clearing state or returning empty shapes.
/// The HWND and its last presented surface remain available for the next open.
#[cfg(windows)]
pub fn hide_launcher(title: &str) -> bool {
    use windows_sys::Win32::UI::WindowsAndMessaging::{SW_HIDE, ShowWindow};
    let Some(hwnd) = owned_launcher(title) else {
        return false;
    };
    unsafe {
        ShowWindow(hwnd, SW_HIDE);
    }
    true
}
#[cfg(not(windows))]
pub fn hide_launcher(_: &str) -> bool {
    false
}

/// Read-only, same-process hook for the native screenshot/lifecycle harness.
#[cfg(windows)]
pub fn launcher_visible(title: &str) -> Option<bool> {
    let hwnd = owned_launcher(title)?;
    Some(unsafe { windows_sys::Win32::UI::WindowsAndMessaging::IsWindowVisible(hwnd) } != 0)
}
#[cfg(not(windows))]
pub fn launcher_visible(_: &str) -> Option<bool> {
    None
}

/// Same-process identity for the opt-in lifecycle probe; never claims a foreign HWND.
#[cfg(windows)]
pub fn launcher_window(title: &str) -> Option<isize> {
    owned_launcher(title).map(|window| window as isize)
}
#[cfg(not(windows))]
pub fn launcher_window(_: &str) -> Option<isize> {
    None
}

/// Called only after the immediate renderer has presented the prepared frame.
#[cfg(windows)]
pub fn focus_launcher(title: &str) -> bool {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        GetForegroundWindow, IsWindowVisible, SetForegroundWindow,
    };
    let Some(hwnd) = owned_launcher(title) else {
        return false;
    };
    if unsafe { IsWindowVisible(hwnd) } == 0 {
        return false;
    }
    unsafe {
        SetForegroundWindow(hwnd);
        GetForegroundWindow() == hwnd
    }
}
#[cfg(not(windows))]
pub fn focus_launcher(_: &str) -> bool {
    false
}

#[cfg(windows)]
pub fn place_launcher(
    title: &str,
    previous: isize,
    override_monitor: Option<crate::performance::monitor::Monitor>,
) -> bool {
    use std::ptr;
    use windows_sys::Win32::{Foundation::RECT, Graphics::Dwm::*, UI::WindowsAndMessaging::*};
    let Some(hwnd) = owned_launcher(title) else {
        return false;
    };
    let mut window = RECT::default();
    if unsafe { GetWindowRect(hwnd, &mut window) } == 0 {
        return false;
    }
    let Some([x, y, width, height]) = placement::install(
        hwnd,
        previous,
        override_monitor,
        [window.right - window.left, window.bottom - window.top],
    ) else {
        return false;
    };
    let mut flags = SWP_NOACTIVATE | SWP_NOZORDER | SWP_NOOWNERZORDER;
    if width == window.right - window.left && height == window.bottom - window.top {
        flags |= SWP_NOSIZE;
    }
    if x == window.left && y == window.top {
        flags |= SWP_NOMOVE;
    }
    unsafe {
        // A command launcher should disappear as soon as it is dismissed.
        // System hide/show transitions can otherwise retain an emptied surface.
        let disabled = 1_i32;
        DwmSetWindowAttribute(
            hwnd,
            DWMWA_TRANSITIONS_FORCEDISABLED as u32,
            (&disabled as *const i32).cast(),
            std::mem::size_of::<i32>() as u32,
        );
        SetWindowPos(hwnd, ptr::null_mut(), x, y, width, height, flags) != 0
    }
}
#[cfg(windows)]
pub fn launcher_bounds(title: &str) -> Option<[i32; 4]> {
    let hwnd = owned_launcher(title)?;
    launcher_bounds_from_handle(hwnd)
}
#[cfg(windows)]
fn launcher_bounds_from_handle(hwnd: windows_sys::Win32::Foundation::HWND) -> Option<[i32; 4]> {
    use windows_sys::Win32::{Foundation::RECT, UI::WindowsAndMessaging::*};
    let mut rect = RECT::default();
    (unsafe { GetWindowRect(hwnd, &mut rect) } != 0).then_some([
        rect.left,
        rect.top,
        rect.right,
        rect.bottom,
    ])
}
/// Same-process regression probe for the launcher's native movement restrictions.
#[cfg(windows)]
pub fn launcher_locked(title: &str) -> Option<bool> {
    owned_launcher(title).map(placement::locked)
}
#[cfg(not(windows))]
pub fn launcher_locked(_: &str) -> Option<bool> {
    None
}
#[cfg(windows)]
pub fn launcher_monitor_selection(
    previous: isize,
    monitors: &[crate::performance::monitor::Monitor],
) -> bool {
    placement::selection_follows_point(previous, monitors)
}
#[cfg(not(windows))]
pub fn launcher_monitor_selection(_: isize, _: &[crate::performance::monitor::Monitor]) -> bool {
    false
}
#[cfg(not(windows))]
pub fn launcher_bounds(_: &str) -> Option<[i32; 4]> {
    None
}
#[cfg(not(windows))]
pub fn place_launcher(_: &str, _: isize, _: Option<crate::performance::monitor::Monitor>) -> bool {
    true
}
impl Drop for Native {
    fn drop(&mut self) {
        self.state.stopped.store(true, Ordering::Release);
        imp::stop(self.state.hwnd.load(Ordering::Acquire));
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

pub fn parse_hotkey(text: &str) -> Option<(u32, u32)> {
    let (mut modifiers, mut key) = (0, None);
    for part in text.split('+').map(str::trim) {
        let part = part.to_ascii_lowercase();
        match part.as_str() {
            "ctrl" | "control" => modifiers |= 2,
            "alt" => modifiers |= 1,
            "shift" => modifiers |= 4,
            "win" | "super" => modifiers |= 8,
            value => {
                if key.is_some() {
                    return None;
                }
                key = Some(match value {
                    "space" => 0x20,
                    "tab" => 9,
                    "enter" => 13,
                    "escape" | "esc" => 27,
                    "backspace" => 8,
                    "insert" => 0x2d,
                    "delete" => 0x2e,
                    "home" => 0x24,
                    "end" => 0x23,
                    "pageup" => 0x21,
                    "pagedown" => 0x22,
                    "arrowleft" => 0x25,
                    "arrowup" => 0x26,
                    "arrowright" => 0x27,
                    "arrowdown" => 0x28,
                    "minus" => 0xbd,
                    "equals" => 0xbb,
                    "comma" => 0xbc,
                    "period" => 0xbe,
                    "slash" => 0xbf,
                    "backslash" => 0xdc,
                    "semicolon" => 0xba,
                    "quote" => 0xde,
                    "backtick" => 0xc0,
                    "openbracket" => 0xdb,
                    "closebracket" => 0xdd,
                    value if value.len() == 1 && value.as_bytes()[0].is_ascii_alphanumeric() => {
                        value.as_bytes()[0].to_ascii_uppercase() as u32
                    }
                    value if value.starts_with('f') => {
                        let n: u32 = value[1..].parse().ok()?;
                        if !(1..=24).contains(&n) {
                            return None;
                        }
                        0x70 + n - 1
                    }
                    _ => return None,
                });
            }
        }
    }
    if modifiers == 0 {
        return None;
    }
    Some((modifiers | 0x4000, key?))
}

/// Two alternating registration IDs let Windows reject a replacement without
/// unregistering the working shortcut. Kept pure for conflict regression tests.
fn replace_hotkey(
    current: &mut Option<(i32, u32, u32)>,
    desired: Option<(u32, u32)>,
    base: i32,
    mut register: impl FnMut(i32, u32, u32) -> bool,
    mut unregister: impl FnMut(i32),
) -> bool {
    let Some((modifiers, key)) = desired else {
        if let Some((id, _, _)) = current.take() {
            unregister(id);
        }
        return true;
    };
    if current
        .is_some_and(|(_, old_modifiers, old_key)| (old_modifiers, old_key) == (modifiers, key))
    {
        return true;
    }
    let next = if current.is_some_and(|(id, _, _)| id == base) {
        base + 1
    } else {
        base
    };
    if !register(next, modifiers, key) {
        return false;
    }
    if let Some((old, _, _)) = current.replace((next, modifiers, key)) {
        unregister(old);
    }
    true
}

#[cfg(windows)]
mod imp {
    use super::*;
    use std::{cell::Cell, ffi::c_void, ptr};
    use windows_sys::Win32::{
        Foundation::*,
        Graphics::Gdi::*,
        System::{DataExchange::*, LibraryLoader::GetModuleHandleW},
        UI::{Input::KeyboardAndMouse::*, Shell::*, WindowsAndMessaging::*},
    };
    const TRAY: u32 = WM_APP + 0x464;
    const RECONFIGURE: u32 = WM_APP + 0x465;
    const HOOK_HOTKEY: u32 = WM_APP + 0x467;
    const HOTKEY: i32 = 0x4649;
    const ICON: u32 = 1;
    /// Marks the key tap that keeps a released Win key from opening Start, so
    /// neither Foldex hook treats it as user input.
    const MASK_MARKER: usize = 0x464f_4c59;
    fn wide(text: &str) -> Vec<u16> {
        text.encode_utf16().chain(Some(0)).collect()
    }
    struct WindowData {
        state: Arc<State>,
        icon: HICON,
        taskbar: u32,
        version4: Cell<bool>,
        hotkey: Cell<Option<(i32, u32, u32)>>,
        win_hook: Cell<HHOOK>,
        clipboard_listener: Cell<bool>,
        tray_allowed: bool,
    }

    // The low-level hook runs on the tray thread that installed it.
    thread_local! {
        static WIN_CHORD: Cell<Option<(HWND, u32, u32)>> = const { Cell::new(None) };
        static WIN_TRIGGER_DOWN: Cell<bool> = const { Cell::new(false) };
    }

    /// Windows reserves most Win chords (Win+Space switches input language):
    /// RegisterHotKey fails or the shell acts first. A low-level hook sees the
    /// keys before shell hotkey processing and can claim them.
    fn hook_win_chord(data: &WindowData, hwnd: HWND, modifiers: u32, key: u32) -> bool {
        if data.win_hook.get().is_null() {
            let hook = unsafe {
                SetWindowsHookExW(
                    WH_KEYBOARD_LL,
                    Some(win_chord_hook),
                    GetModuleHandleW(ptr::null()),
                    0,
                )
            };
            if hook.is_null() {
                return false;
            }
            data.win_hook.set(hook);
        }
        WIN_CHORD.set(Some((hwnd, modifiers & 0xf, key)));
        WIN_TRIGGER_DOWN.set(false);
        true
    }

    fn unhook_win_chord(data: &WindowData) {
        let hook = data.win_hook.replace(ptr::null_mut());
        if !hook.is_null() {
            unsafe {
                UnhookWindowsHookEx(hook);
            }
        }
        WIN_CHORD.set(None);
        WIN_TRIGGER_DOWN.set(false);
    }

    fn held_modifiers() -> u32 {
        let down = |vk: u16| unsafe { GetAsyncKeyState(vk as i32) } < 0;
        (down(VK_MENU) as u32 * MOD_ALT)
            | (down(VK_CONTROL) as u32 * MOD_CONTROL)
            | (down(VK_SHIFT) as u32 * MOD_SHIFT)
            | ((down(VK_LWIN) || down(VK_RWIN)) as u32 * MOD_WIN)
    }

    /// A Win press followed only by its release opens Start. Inject an unmapped
    /// key between them, as keyboard remappers do.
    fn mask_start_menu() {
        let key = |flags| INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 {
                ki: KEYBDINPUT {
                    wVk: 0xe8,
                    wScan: 0,
                    dwFlags: flags,
                    time: 0,
                    dwExtraInfo: MASK_MARKER,
                },
            },
        };
        let inputs = [key(0), key(KEYEVENTF_KEYUP)];
        unsafe {
            SendInput(
                inputs.len() as u32,
                inputs.as_ptr(),
                std::mem::size_of::<INPUT>() as i32,
            );
        }
    }

    unsafe extern "system" fn win_chord_hook(code: i32, message: WPARAM, data: LPARAM) -> LRESULT {
        if code == HC_ACTION as i32
            && data != 0
            && let Some((hwnd, modifiers, vk)) = WIN_CHORD.get()
        {
            // Windows owns this structure for the duration of this callback.
            let key = unsafe { &*(data as *const KBDLLHOOKSTRUCT) };
            if key.vkCode == vk && key.dwExtraInfo != MASK_MARKER {
                let pressed = matches!(message as u32, WM_KEYDOWN | WM_SYSKEYDOWN);
                if pressed {
                    if held_modifiers() == modifiers {
                        // Repeats of a claimed press are swallowed without toggling.
                        if !WIN_TRIGGER_DOWN.replace(true) {
                            mask_start_menu();
                            unsafe {
                                PostMessageW(hwnd, HOOK_HOTKEY, 0, 0);
                            }
                        }
                        return 1;
                    }
                    WIN_TRIGGER_DOWN.set(false);
                } else if WIN_TRIGGER_DOWN.replace(false) {
                    return 1;
                }
            }
        }
        unsafe { CallNextHookEx(ptr::null_mut(), code, message, data) }
    }
    fn tray(data: &WindowData, hwnd: HWND, add: bool) -> bool {
        let mut icon = NOTIFYICONDATAW {
            cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
            hWnd: hwnd,
            uID: ICON,
            uFlags: NIF_MESSAGE | NIF_ICON | NIF_TIP | NIF_SHOWTIP,
            uCallbackMessage: TRAY,
            hIcon: data.icon,
            ..Default::default()
        };
        let tip = wide("Foldex");
        icon.szTip[..tip.len()].copy_from_slice(&tip);
        let success =
            unsafe { Shell_NotifyIconW(if add { NIM_ADD } else { NIM_DELETE }, &mut icon) } != 0;
        if add {
            icon.Anonymous.uVersion = 4;
            data.version4
                .set(unsafe { Shell_NotifyIconW(NIM_SETVERSION, &mut icon) } != 0);
        }
        data.state
            .tray_ready
            .store(add && success, Ordering::Release);
        success
    }
    fn configure_window(data: &WindowData, hwnd: HWND) {
        let mut error = None;
        let enabled = data.state.enabled.load(Ordering::Acquire);
        let hotkey = data
            .state
            .hotkey
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let mut shortcut_error = None;
        let mut current = data.hotkey.get();
        if enabled {
            if data.tray_allowed
                && !data.state.tray_ready.load(Ordering::Acquire)
                && !tray(data, hwnd, true)
            {
                error = Some("Windows could not add Foldex to the system tray".into());
            }
            if let Some((modifiers, key)) =
                parse_hotkey(&hotkey).filter(|(modifiers, _)| modifiers & MOD_WIN != 0)
            {
                if hook_win_chord(data, hwnd, modifiers, key) {
                    replace_hotkey(
                        &mut current,
                        None,
                        HOTKEY,
                        |_, _, _| false,
                        |id| unsafe {
                            UnregisterHotKey(hwnd, id);
                        },
                    );
                } else {
                    shortcut_error = Some(format!(
                        "Windows could not reserve {hotkey}. Your previous shortcut is still active."
                    ));
                }
            } else if hotkey.is_empty() || parse_hotkey(&hotkey).is_some() {
                if replace_hotkey(
                    &mut current,
                    parse_hotkey(&hotkey),
                    HOTKEY,
                    |id, modifiers, key| unsafe { RegisterHotKey(hwnd, id, modifiers, key) != 0 },
                    |id| unsafe {
                        UnregisterHotKey(hwnd, id);
                    },
                ) {
                    unhook_win_chord(data);
                } else {
                    shortcut_error = Some(format!(
                        "{hotkey} is already in use. Your previous shortcut is still active."
                    ));
                }
            } else {
                shortcut_error =
                    Some("Record a shortcut with Ctrl, Alt, Shift or Win and one key.".into());
            }
        } else {
            if data.state.tray_ready.load(Ordering::Acquire) {
                tray(data, hwnd, false);
            }
            unhook_win_chord(data);
            replace_hotkey(
                &mut current,
                None,
                HOTKEY,
                |_, _, _| false,
                |id| unsafe {
                    UnregisterHotKey(hwnd, id);
                },
            );
        }
        data.hotkey.set(current);
        {
            let mut feedback = data
                .state
                .hotkey_feedback
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            // A newer configure may have arrived while Windows registered this
            // one. Keep its pending request; the queued message will settle it.
            if feedback.requested == hotkey {
                if shortcut_error.is_none() {
                    feedback.active = (current.is_some() || !data.win_hook.get().is_null())
                        .then(|| hotkey.clone());
                }
                feedback.error = shortcut_error;
                feedback.pending = false;
            }
        }
        let clipboard = data.state.clipboard.load(Ordering::Acquire);
        if clipboard != data.clipboard_listener.get() {
            if clipboard {
                if unsafe { AddClipboardFormatListener(hwnd) } != 0 {
                    data.clipboard_listener.set(true);
                } else {
                    error = Some("Windows could not enable clipboard notifications".into());
                }
            } else {
                unsafe {
                    RemoveClipboardFormatListener(hwnd);
                }
                data.clipboard_listener.set(false);
            }
        }
        *data.state.status.lock().unwrap_or_else(|e| e.into_inner()) = error;
        data.state.event(STATUS);
    }
    unsafe extern "system" fn callback(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
        if msg == WM_NCCREATE {
            let create = unsafe { &*(lp as *const CREATESTRUCTW) };
            unsafe {
                SetWindowLongPtrW(hwnd, GWLP_USERDATA, create.lpCreateParams as isize);
            }
        }
        let pointer = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *const WindowData;
        if pointer.is_null() {
            return unsafe { DefWindowProcW(hwnd, msg, wp, lp) };
        }
        let data = unsafe { &*pointer };
        if data.taskbar != 0 && msg == data.taskbar {
            data.state.tray_ready.store(false, Ordering::Release);
            if data.state.enabled.load(Ordering::Acquire) {
                configure_window(data, hwnd);
            }
            return 0;
        }
        match msg {
            WM_COPYDATA => {
                if lp == 0 {
                    return 0;
                }
                let packet = unsafe { &*(lp as *const COPYDATASTRUCT) };
                if packet.dwData != super::super::instance::PACKET
                    || packet.cbData < 2
                    || packet.cbData as usize > super::super::instance::MAX_UNITS * 2
                    || packet.cbData % 2 != 0
                    || packet.lpData.is_null()
                {
                    return 0;
                }
                // WM_COPYDATA owns valid packet memory only for this callback.
                // Read unaligned units, validate, then retain only our own copy.
                let pointer = packet.lpData as *const u16;
                let units: Vec<_> = (0..packet.cbData as usize / 2)
                    .map(|i| unsafe { pointer.add(i).read_unaligned() })
                    .collect();
                let Some(activation) = super::super::instance::decode(&units) else {
                    return 0;
                };
                let mut queue = data
                    .state
                    .activations
                    .lock()
                    .unwrap_or_else(|e| e.into_inner());
                if queue.len() >= 32 {
                    return 0;
                }
                queue.push_back(activation);
                drop(queue);
                data.state.event(ACTIVATE);
                1
            }
            WM_HOTKEY | HOOK_HOTKEY => {
                let registered = if msg == HOOK_HOTKEY {
                    !data.win_hook.get().is_null()
                } else {
                    data.hotkey
                        .get()
                        .is_some_and(|(id, _, _)| wp == id as usize)
                };
                if !data.state.enabled.load(Ordering::Acquire) || !registered {
                    return 0;
                }
                if !data.state.opened.load(Ordering::Acquire) {
                    data.state.previous.store(foreground(), Ordering::Release);
                }
                data.state.event(TOGGLE);
                0
            }
            WM_CLIPBOARDUPDATE => {
                if data.state.clipboard.load(Ordering::Acquire) {
                    data.state.clipboard_version.fetch_add(1, Ordering::Release);
                    let _ = data.state.signal.try_send(());
                }
                0
            }
            RECONFIGURE => {
                configure_window(data, hwnd);
                0
            }
            TRAY => {
                if !data.state.enabled.load(Ordering::Acquire) {
                    return 0;
                }
                let event = lp as u32 & 0xffff;
                let version4 = data.version4.get();
                if if version4 {
                    event == NIN_SELECT || event == (NIN_SELECT | NINF_KEY)
                } else {
                    event == WM_LBUTTONUP
                } {
                    data.state.previous.store(foreground(), Ordering::Release);
                    data.state.event(TOGGLE);
                } else if if version4 {
                    event == WM_CONTEXTMENU
                } else {
                    event == WM_RBUTTONUP
                } {
                    let menu = unsafe { CreatePopupMenu() };
                    if !menu.is_null() {
                        for (id, title) in [
                            (1, "Open Foldex"),
                            (2, "Open launcher"),
                            (3, "Launcher settings"),
                            (4, "Exit Foldex"),
                        ] {
                            let title = wide(title);
                            unsafe {
                                AppendMenuW(menu, MF_STRING, id, title.as_ptr());
                            }
                        }
                        let previous = foreground();
                        let mut point = POINT::default();
                        unsafe {
                            GetCursorPos(&mut point);
                            SetForegroundWindow(hwnd);
                        }
                        let selected = unsafe {
                            TrackPopupMenuEx(
                                menu,
                                TPM_RETURNCMD | TPM_NONOTIFY | TPM_RIGHTBUTTON,
                                point.x,
                                point.y,
                                hwnd,
                                ptr::null(),
                            )
                        };
                        unsafe {
                            DestroyMenu(menu);
                            PostMessageW(hwnd, WM_NULL, 0, 0);
                        }
                        match selected as u32 {
                            1 => data.state.event(EXPLORER),
                            2 => {
                                data.state.previous.store(previous, Ordering::Release);
                                data.state.event(TOGGLE);
                            }
                            3 => {
                                data.state.previous.store(previous, Ordering::Release);
                                data.state.event(SETTINGS);
                            }
                            4 => data.state.event(EXIT),
                            _ => {}
                        }
                    }
                }
                0
            }
            WM_CLOSE => {
                unsafe {
                    DestroyWindow(hwnd);
                }
                0
            }
            WM_DESTROY => {
                if data.tray_allowed {
                    tray(data, hwnd, false);
                }
                unsafe {
                    if let Some((id, _, _)) = data.hotkey.take() {
                        UnregisterHotKey(hwnd, id);
                    }
                    unhook_win_chord(data);
                    RemoveClipboardFormatListener(hwnd);
                    PostQuitMessage(0);
                }
                0
            }
            _ => unsafe { DefWindowProcW(hwnd, msg, wp, lp) },
        }
    }
    fn app_icon() -> HICON {
        // Pick the closest embedded ICO frame for the current taskbar metric.
        // This is an owned icon: WindowData destroys it when the tray closes.
        let size = unsafe { GetSystemMetrics(SM_CXSMICON) }.max(16);
        let resource = unsafe {
            LoadImageW(
                GetModuleHandleW(ptr::null()),
                1_usize as *const u16,
                IMAGE_ICON,
                size,
                size,
                LR_DEFAULTCOLOR,
            )
        };
        if !resource.is_null() {
            return resource.cast();
        }
        // Same bitmap as the explorer window, with an alpha DIB and owned mask.
        let image = crate::app_icon();
        let width = image.width as i32;
        let height = image.height as i32;
        let info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: width,
                biHeight: -height,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut bits: *mut c_void = ptr::null_mut();
        let bitmap = unsafe {
            CreateDIBSection(
                ptr::null_mut(),
                &info,
                DIB_RGB_COLORS,
                &mut bits,
                ptr::null_mut(),
                0,
            )
        };
        if bitmap.is_null() || bits.is_null() {
            if !bitmap.is_null() {
                unsafe {
                    DeleteObject(bitmap);
                }
            }
            return unsafe { CopyIcon(LoadIconW(ptr::null_mut(), IDI_APPLICATION)) };
        }
        let pixels = unsafe { std::slice::from_raw_parts_mut(bits as *mut u8, image.rgba.len()) };
        for (source, target) in image.rgba.chunks_exact(4).zip(pixels.chunks_exact_mut(4)) {
            target.copy_from_slice(&[source[2], source[1], source[0], source[3]]);
        }
        let mask_bits = vec![0_u8; ((width as usize + 15) / 16) * 2 * height as usize];
        let mask = unsafe { CreateBitmap(width, height, 1, 1, mask_bits.as_ptr().cast()) };
        let icon = unsafe {
            CreateIconIndirect(&ICONINFO {
                fIcon: 1,
                xHotspot: 0,
                yHotspot: 0,
                hbmMask: mask,
                hbmColor: bitmap,
            })
        };
        unsafe {
            DeleteObject(bitmap);
            if !mask.is_null() {
                DeleteObject(mask);
            }
        }
        icon
    }
    pub fn run(state: Arc<State>) {
        run_with_tray(state, true);
    }

    fn run_with_tray(state: Arc<State>, tray_allowed: bool) {
        if state.stopped.load(Ordering::Acquire) {
            return;
        }
        let name = wide(
            &state
                .channel
                .clone()
                .unwrap_or_else(|| format!("Foldex.Background.{}", std::process::id())),
        );
        let instance = unsafe { GetModuleHandleW(ptr::null()) };
        let class = WNDCLASSW {
            lpfnWndProc: Some(callback),
            hInstance: instance,
            lpszClassName: name.as_ptr(),
            ..Default::default()
        };
        if unsafe { RegisterClassW(&class) } == 0 {
            *state.status.lock().unwrap_or_else(|e| e.into_inner()) =
                Some("Unable to start the launcher message window".into());
            state.event(STATUS);
            return;
        }
        let mut data = Box::new(WindowData {
            state: state.clone(),
            icon: app_icon(),
            taskbar: unsafe { RegisterWindowMessageW(wide("TaskbarCreated").as_ptr()) },
            version4: Cell::new(false),
            hotkey: Cell::new(None),
            win_hook: Cell::new(ptr::null_mut()),
            clipboard_listener: Cell::new(false),
            tray_allowed,
        });
        let hwnd = unsafe {
            CreateWindowExW(
                WS_EX_TOOLWINDOW,
                name.as_ptr(),
                wide("Foldex background").as_ptr(),
                0,
                0,
                0,
                0,
                0,
                ptr::null_mut(),
                ptr::null_mut(),
                instance,
                (&mut *data as *mut WindowData).cast(),
            )
        };
        if hwnd.is_null() {
            *state.status.lock().unwrap_or_else(|e| e.into_inner()) =
                Some("Unable to create the launcher message window".into());
            state.event(STATUS);
        } else {
            state.hwnd.store(hwnd as isize, Ordering::Release);
            configure_window(&data, hwnd);
            if state.stopped.load(Ordering::Acquire) {
                unsafe {
                    PostMessageW(hwnd, WM_CLOSE, 0, 0);
                }
            }
            let mut message = MSG::default();
            while unsafe { GetMessageW(&mut message, ptr::null_mut(), 0, 0) } > 0 {
                unsafe {
                    TranslateMessage(&message);
                    DispatchMessageW(&message);
                }
            }
            state.hwnd.store(0, Ordering::Release);
            unsafe {
                if IsWindow(hwnd) != 0 {
                    DestroyWindow(hwnd);
                }
            }
        }
        unsafe {
            if !data.icon.is_null() {
                DestroyIcon(data.icon);
            }
            UnregisterClassW(name.as_ptr(), instance);
        }
    }
    pub fn configure(hwnd: isize) {
        if hwnd != 0 {
            unsafe {
                PostMessageW(hwnd as _, RECONFIGURE, 0, 0);
            }
        }
    }
    pub fn stop(hwnd: isize) {
        if hwnd != 0 {
            unsafe {
                PostMessageW(hwnd as _, WM_CLOSE, 0, 0);
            }
        }
    }
    pub fn foreground() -> isize {
        let hwnd = unsafe { GetForegroundWindow() };
        if !hwnd.is_null() && unsafe { IsWindowVisible(hwnd) } != 0 {
            hwnd as isize
        } else {
            0
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
        use windows_sys::Win32::System::{StationsAndDesktops::*, Threading::GetCurrentThreadId};

        fn object_name(handle: HANDLE) -> String {
            let mut units = [0u16; 256];
            let mut required = 0;
            assert_ne!(
                unsafe {
                    GetUserObjectInformationW(
                        handle,
                        UOI_NAME,
                        units.as_mut_ptr().cast(),
                        std::mem::size_of_val(&units) as u32,
                        &mut required,
                    )
                },
                0,
                "read desktop/window-station name: {}",
                std::io::Error::last_os_error()
            );
            let end = units
                .iter()
                .position(|unit| *unit == 0)
                .expect("bounded native object name");
            String::from_utf16_lossy(&units[..end])
        }

        struct InputDesktop(HDESK);
        impl Drop for InputDesktop {
            fn drop(&mut self) {
                unsafe {
                    CloseDesktop(self.0);
                }
            }
        }

        struct HeldHotkeys {
            window: HWND,
            registrations: Vec<i32>,
        }
        impl HeldHotkeys {
            fn new() -> Self {
                let class = wide("STATIC");
                let window = unsafe {
                    CreateWindowExW(
                        WS_EX_TOOLWINDOW,
                        class.as_ptr(),
                        wide("Foldex owned hotkey conflict holder").as_ptr(),
                        0,
                        0,
                        0,
                        0,
                        0,
                        ptr::null_mut(),
                        ptr::null_mut(),
                        ptr::null_mut(),
                        ptr::null(),
                    )
                };
                assert!(
                    !window.is_null(),
                    "create owned hidden HWND: {}",
                    std::io::Error::last_os_error()
                );
                Self {
                    window,
                    registrations: Vec::with_capacity(4),
                }
            }
            fn register(&mut self, id: i32, chord: (u32, u32)) -> bool {
                assert!(
                    !self.registrations.contains(&id),
                    "test never reuses a live registration ID"
                );
                if unsafe { RegisterHotKey(self.window, id, chord.0, chord.1) } == 0 {
                    return false;
                }
                self.registrations.push(id);
                true
            }
            fn release(&mut self, id: i32) {
                let at = self
                    .registrations
                    .iter()
                    .position(|held| *held == id)
                    .expect("release an owned registration only");
                assert_ne!(
                    unsafe { UnregisterHotKey(self.window, id) },
                    0,
                    "release owned hotkey"
                );
                self.registrations.swap_remove(at);
            }
        }
        impl Drop for HeldHotkeys {
            fn drop(&mut self) {
                for id in self.registrations.drain(..) {
                    unsafe {
                        UnregisterHotKey(self.window, id);
                    }
                }
                unsafe {
                    DestroyWindow(self.window);
                }
            }
        }

        fn settled_feedback(native: &Native, requested: &str) -> HotkeyFeedback {
            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                let feedback = native.hotkey_feedback();
                if feedback.requested == requested
                    && !feedback.pending
                    && native.state.events.load(Ordering::Acquire) & STATUS != 0
                {
                    return feedback;
                }
                assert!(
                    Instant::now() < deadline,
                    "native shortcut configuration deadline: {feedback:?}"
                );
                assert!(
                    !native
                        .thread
                        .as_ref()
                        .is_some_and(std::thread::JoinHandle::is_finished),
                    "native message thread stopped early"
                );
                std::thread::sleep(Duration::from_millis(2));
            }
        }

        #[test]
        #[ignore = "requires FOLDEX_NATIVE_HOTKEY_PROBE=1 on a driver-owned offscreen WinSta0 desktop; never sends input"]
        fn native_hotkey_conflict_and_rollback_without_tray_or_clipboard() {
            assert_eq!(
                std::env::var("FOLDEX_NATIVE_HOTKEY_PROBE").as_deref(),
                Ok("1"),
                "explicit hotkey-probe opt-in required"
            );
            let station = object_name(unsafe { GetProcessWindowStation() });
            assert!(
                station.eq_ignore_ascii_case("WinSta0"),
                "probe expects an offscreen desktop in WinSta0, got {station}"
            );
            let desktop = unsafe { GetThreadDesktop(GetCurrentThreadId()) };
            assert!(!desktop.is_null());
            let desktop_name = object_name(desktop);
            assert!(
                desktop_name.starts_with("Foldex") && !desktop_name.eq_ignore_ascii_case("Default"),
                "probe requires a driver-owned Foldex offscreen desktop, got {desktop_name}"
            );
            let input = unsafe { OpenInputDesktop(0, 0, DESKTOP_READOBJECTS) };
            assert!(
                !input.is_null(),
                "must identify active input desktop before registering any probe chord"
            );
            let input = InputDesktop(input);
            let input_name = object_name(input.0);
            assert!(
                !desktop_name.eq_ignore_ascii_case(&input_name),
                "refusing to probe the input desktop {input_name}"
            );
            drop(input);

            const WORKING: &str = "Ctrl+Alt+Shift+F23";
            const CANDIDATE: &str = "Ctrl+Alt+Shift+F24";
            const BLOCKED_ID: i32 = 0x7141;
            const VERIFY_OLD_ID: i32 = 0x7142;
            const VERIFY_NEW_ID: i32 = 0x7143;
            let old_chord = parse_hotkey(WORKING).unwrap();
            let new_chord = parse_hotkey(CANDIDATE).unwrap();
            let mut held = HeldHotkeys::new();
            // Never unregister a pre-existing registration. An unavailable
            // obscure test chord fails this opt-in test before Native starts.
            assert!(
                held.register(BLOCKED_ID, new_chord),
                "probe candidate must be free for our owned holder"
            );
            let (signal, _) = std::sync::mpsc::sync_channel(1);
            let channel = format!(
                "Foldex.HotkeyProbe.{}.{}",
                std::process::id(),
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            );
            let mut native = Native::new(
                false,
                false,
                WORKING.into(),
                Arc::new(|| {}),
                signal,
                true,
                Some(channel),
            )
            .unwrap();
            let state = native.state.clone();
            let thread_desktop = desktop as isize;
            native.thread = Some(
                std::thread::Builder::new()
                    .name("foldex-hotkey-probe".into())
                    .spawn(move || {
                        assert_ne!(
                            unsafe { SetThreadDesktop(thread_desktop as HDESK) },
                            0,
                            "attach owned offscreen desktop"
                        );
                        run_with_tray(state, false);
                    })
                    .unwrap(),
            );
            let deadline = Instant::now() + Duration::from_secs(5);
            while native.state.window() == 0
                || native.state.events.load(Ordering::Acquire) & STATUS == 0
            {
                assert!(
                    Instant::now() < deadline,
                    "native message-window startup deadline"
                );
                assert!(
                    !native.thread.as_ref().unwrap().is_finished(),
                    "native message-window startup failed"
                );
                std::thread::sleep(Duration::from_millis(2));
            }
            native.take_events();
            native.configure(true, false, WORKING);
            let initial = settled_feedback(&native, WORKING);
            assert_eq!(initial.active.as_deref(), Some(WORKING));
            assert!(initial.error.is_none(), "initial binding: {initial:?}");
            assert!(!native.state.tray_ready.load(Ordering::Acquire));
            assert!(!native.state.remembers_clipboard());
            native.take_events();

            native.configure(true, false, CANDIDATE);
            let rejected = settled_feedback(&native, CANDIDATE);
            assert!(!rejected.pending);
            assert_eq!(rejected.active.as_deref(), Some(WORKING));
            let inline_error = rejected
                .error
                .as_deref()
                .expect("real Windows conflict reports inline shortcut error");
            assert!(
                inline_error.contains(CANDIDATE) && inline_error.contains("previous shortcut"),
                "{inline_error}"
            );
            assert!(
                native
                    .state
                    .status
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .is_none(),
                "shortcut conflicts must stay out of the app-global service error domain"
            );
            assert_ne!(
                native.take_events() & STATUS,
                0,
                "completion wakes Options without a global error payload"
            );
            assert!(
                !held.register(VERIFY_OLD_ID, old_chord),
                "real old registration must still occupy its chord"
            );

            held.release(BLOCKED_ID);
            native.configure(true, false, CANDIDATE);
            let accepted = settled_feedback(&native, CANDIDATE);
            assert_eq!(accepted.active.as_deref(), Some(CANDIDATE));
            assert!(
                accepted.error.is_none(),
                "retry after releasing own blocker: {accepted:?}"
            );
            assert!(
                held.register(VERIFY_OLD_ID, old_chord),
                "successful replacement releases old registration"
            );
            held.release(VERIFY_OLD_ID);
            assert!(
                !held.register(VERIFY_NEW_ID, new_chord),
                "accepted candidate is registered by Native"
            );

            native.take_events();
            native.configure(false, false, CANDIDATE);
            let deadline = Instant::now() + Duration::from_secs(5);
            while native.state.events.load(Ordering::Acquire) & STATUS == 0 {
                assert!(
                    Instant::now() < deadline,
                    "disabled registration cleanup deadline"
                );
                std::thread::sleep(Duration::from_millis(2));
            }
            let disabled = native.hotkey_feedback();
            assert!(!disabled.pending && disabled.active.is_none() && disabled.error.is_none());
            let state = native.state.clone();
            drop(native); // Stops/joins the actual message thread; destroys its HWND.
            assert_eq!(state.window(), 0);
            assert!(!state.tray_ready.load(Ordering::Acquire));
            assert!(!state.remembers_clipboard());
            assert_eq!(state.clipboard_version.load(Ordering::Acquire), 0);
            assert!(
                state
                    .status
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .is_none()
            );
            assert!(
                held.register(VERIFY_OLD_ID, old_chord),
                "old chord is available after cleanup"
            );
            assert!(
                held.register(VERIFY_NEW_ID, new_chord),
                "candidate chord is available after cleanup"
            );
            let holder_window = held.window;
            drop(held); // Releases only our verified registrations and our HWND.
            assert_eq!(unsafe { IsWindow(holder_window) }, 0);
            eprintln!(
                "FOLDEX_NATIVE_HOTKEY_PROBE {}",
                serde_json::json!({
                    "station":station,"offscreen_desktop":desktop_name,"input_desktop":input_name,
                    "working":WORKING,"rejected_candidate":CANDIDATE,"inline_error":inline_error,
                    "old_active_retained":rejected.active,"pending_cleared":!rejected.pending,
                    "global_service_error":false,"retry_active":accepted.active,
                    "owned_registrations_released":true,"owned_windows_destroyed":true,
                    "tray":false,"clipboard":false,"input_sent":false,"passed":true
                })
            );
        }
    }
}
#[cfg(not(windows))]
mod imp {
    use super::*;
    pub fn run(_: Arc<State>) {}
    pub fn configure(_: isize) {}
    pub fn stop(_: isize) {}
    pub fn foreground() -> isize {
        0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn shortcut_parser_requires_modifiers_and_one_key() {
        assert_eq!(parse_hotkey("Ctrl+Alt+Space"), Some((0x4003, 0x20)));
        assert_eq!(parse_hotkey("Ctrl+Shift+F24"), Some((0x4006, 0x87)));
        for bad in ["Space", "Ctrl", "Ctrl+Alt+", "Ctrl+F25", "Ctrl+A+B"] {
            assert!(parse_hotkey(bad).is_none());
        }
    }
    #[test]
    fn shortcut_conflict_keeps_working_registration_and_success_releases_after_register() {
        use std::cell::RefCell;
        let old = (100, 0x4003, 0x20);
        let mut active = Some(old);
        let calls = RefCell::new(Vec::new());
        assert!(!replace_hotkey(
            &mut active,
            Some((0x4006, 0x4b)),
            100,
            |id, _, _| {
                calls.borrow_mut().push((true, id));
                false
            },
            |id| calls.borrow_mut().push((false, id))
        ));
        assert_eq!(active, Some(old));
        assert_eq!(*calls.borrow(), [(true, 101)]);
        calls.borrow_mut().clear();
        assert!(replace_hotkey(
            &mut active,
            Some((0x4006, 0x4b)),
            100,
            |id, _, _| {
                calls.borrow_mut().push((true, id));
                true
            },
            |id| calls.borrow_mut().push((false, id))
        ));
        assert_eq!(active, Some((101, 0x4006, 0x4b)));
        assert_eq!(*calls.borrow(), [(true, 101), (false, 100)]);
        calls.borrow_mut().clear();
        assert!(replace_hotkey(
            &mut active,
            None,
            100,
            |_, _, _| panic!("clear never registers"),
            |id| calls.borrow_mut().push((false, id))
        ));
        assert_eq!(active, None);
        assert_eq!(*calls.borrow(), [(false, 101)]);
    }
    #[cfg(windows)]
    #[test]
    fn private_instance_handoff_is_bounded_and_cleans_up_without_tray_or_input() {
        use super::super::instance::{self, Activation, Claim};
        use std::{
            path::PathBuf,
            time::{Duration, Instant, SystemTime, UNIX_EPOCH},
        };
        let channel = format!(
            "Foldex.Test.{}.{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        let Claim::Primary(guard) = instance::claim(&channel, &Activation::StayInTray).unwrap()
        else {
            panic!("test owns a unique channel")
        };
        let (signal, _) = std::sync::mpsc::sync_channel(1);
        let native = Native::new(
            false,
            false,
            "Ctrl+Alt+Space".into(),
            Arc::new(|| {}),
            signal,
            false,
            Some(channel.clone()),
        )
        .unwrap();
        let deadline = Instant::now() + Duration::from_secs(4);
        while native.state.window() == 0 {
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(5));
        }
        let client = std::thread::spawn(move || {
            for at in 0..32 {
                assert!(matches!(
                    instance::claim(
                        &channel,
                        &Activation::Path(PathBuf::from(format!("C:/Private Ω/{at}")))
                    )
                    .unwrap(),
                    Claim::Forwarded
                ));
            }
            assert!(
                instance::claim(&channel, &Activation::Explorer).is_err(),
                "full inbox refuses more work"
            );
        });
        client.join().unwrap();
        let incoming = native.take_activations();
        assert_eq!(incoming.len(), 32);
        assert_eq!(
            incoming[0],
            Activation::Path(PathBuf::from("C:/Private Ω/0"))
        );
        assert_eq!(
            incoming[31],
            Activation::Path(PathBuf::from("C:/Private Ω/31"))
        );
        let state = native.state.clone();
        drop(native);
        assert_eq!(state.window(), 0);
        drop(guard);
    }
}
