#[cfg(windows)]
mod implementation {
    use std::{io, ptr, thread::Thread};
    use windows_sys::Win32::{
        Foundation::{CloseHandle, HANDLE, WAIT_FAILED},
        System::Threading::{CreateEventW, INFINITE, SetEvent},
        UI::WindowsAndMessaging::{MWMO_INPUTAVAILABLE, MsgWaitForMultipleObjectsEx, QS_ALLINPUT},
    };

    pub(crate) struct Signal(isize);
    impl Signal {
        pub(crate) fn new() -> io::Result<Self> {
            let handle = unsafe { CreateEventW(ptr::null(), 0, 0, ptr::null()) };
            if handle.is_null() {
                Err(io::Error::last_os_error())
            } else {
                Ok(Self(handle as isize))
            }
        }
        pub(crate) fn attach(&self, _: Thread) {}
        pub(crate) fn notify(&self) {
            unsafe { SetEvent(self.0 as HANDLE) };
        }
        pub(crate) fn wait(&self) -> bool {
            let handle = self.0 as HANDLE;
            // Wakes for our auto-reset event or STA messages. No polling timer.
            unsafe {
                MsgWaitForMultipleObjectsEx(1, &handle, INFINITE, QS_ALLINPUT, MWMO_INPUTAVAILABLE)
                    != WAIT_FAILED
            }
        }
    }
    impl Drop for Signal {
        fn drop(&mut self) {
            unsafe { CloseHandle(self.0 as HANDLE) };
        }
    }
}

#[cfg(not(windows))]
mod implementation {
    use std::{
        io,
        sync::OnceLock,
        thread::{self, Thread},
    };
    pub(crate) struct Signal(OnceLock<Thread>);
    impl Signal {
        pub(crate) fn new() -> io::Result<Self> {
            Ok(Self(OnceLock::new()))
        }
        pub(crate) fn attach(&self, thread: Thread) {
            let _ = self.0.set(thread);
        }
        pub(crate) fn notify(&self) {
            if let Some(thread) = self.0.get() {
                thread.unpark();
            }
        }
        pub(crate) fn wait(&self) -> bool {
            thread::park();
            true
        }
    }
}

pub(super) use implementation::Signal;
