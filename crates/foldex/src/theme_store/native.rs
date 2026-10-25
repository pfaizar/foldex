//! Native Unicode file dialogs, called only by the theme worker after a user
//! activates Import or Export. Uses the system API without another UI runtime.
use std::path::PathBuf;

#[cfg(windows)]
mod windows {
    use super::*;
    use std::ffi::c_void;
    use windows_sys::Win32::UI::WindowsAndMessaging::IsWindow;

    #[repr(C)]
    struct OpenFileName {
        size: u32,
        owner: *mut c_void,
        instance: *mut c_void,
        filter: *const u16,
        custom_filter: *mut u16,
        max_custom_filter: u32,
        filter_index: u32,
        file: *mut u16,
        max_file: u32,
        file_title: *mut u16,
        max_file_title: u32,
        initial_dir: *const u16,
        title: *const u16,
        flags: u32,
        file_offset: u16,
        file_extension: u16,
        default_ext: *const u16,
        custom_data: isize,
        hook: usize,
        template_name: *const u16,
        reserved: *mut c_void,
        reserved_dword: u32,
        flags_ex: u32,
    }
    #[link(name = "comdlg32")]
    unsafe extern "system" {
        fn GetOpenFileNameW(options: *mut OpenFileName) -> i32;
        fn GetSaveFileNameW(options: *mut OpenFileName) -> i32;
        fn CommDlgExtendedError() -> u32;
    }
    pub(super) fn choose(owner: isize, name: Option<&str>) -> Result<Option<PathBuf>, String> {
        use std::os::windows::ffi::OsStringExt;
        let owner = owner as *mut c_void;
        let owner = if unsafe { IsWindow(owner) } != 0 {
            owner
        } else {
            std::ptr::null_mut()
        };
        let filter: Vec<u16> = "Foldex themes (*.toml)\0*.toml\0\0"
            .encode_utf16()
            .collect();
        let title: Vec<u16> = if name.is_some() {
            "Export Theme\0"
        } else {
            "Load Themes\0"
        }
        .encode_utf16()
        .collect();
        let extension: Vec<u16> = "toml\0".encode_utf16().collect();
        let mut file = vec![0_u16; 32768];
        if let Some(name) = name {
            for (target, character) in file.iter_mut().zip(format!("{name}.toml").encode_utf16()) {
                *target = character;
            }
        }
        // OPENFILENAMEW contains only scalar values and nullable pointers.
        let mut options: OpenFileName = unsafe { std::mem::zeroed() };
        options.size = std::mem::size_of::<OpenFileName>() as u32;
        options.owner = owner;
        options.filter = filter.as_ptr();
        options.filter_index = 1;
        options.file = file.as_mut_ptr();
        options.max_file = file.len() as u32;
        options.title = title.as_ptr();
        options.default_ext = extension.as_ptr();
        // Explorer layout, no process-directory change, existing parent path,
        // and no Recent Documents entry. Export confirms native overwrites.
        options.flags = 0x0008_0000
            | 0x0000_0008
            | 0x0000_0800
            | 0x0200_0000
            | if name.is_some() {
                0x0000_0002
            } else {
                0x0000_1000
            };
        let accepted = unsafe {
            if name.is_some() {
                GetSaveFileNameW(&mut options)
            } else {
                GetOpenFileNameW(&mut options)
            }
        };
        if accepted == 0 {
            let error = unsafe { CommDlgExtendedError() };
            return if error == 0 {
                Ok(None)
            } else {
                Err(format!("The native theme picker failed (0x{error:04x})."))
            };
        }
        let end = file
            .iter()
            .position(|character| *character == 0)
            .ok_or("The native picker returned an invalid path.")?;
        let path = PathBuf::from(std::ffi::OsString::from_wide(&file[..end]));
        if !path.is_absolute() {
            return Err("The native picker must return an absolute path.".into());
        }
        Ok(Some(path))
    }
}

pub(super) fn open(owner: isize) -> Result<Option<PathBuf>, String> {
    #[cfg(windows)]
    {
        windows::choose(owner, None)
    }
    #[cfg(not(windows))]
    {
        let _ = owner;
        Err("Native theme import is available on Windows.".into())
    }
}
pub(super) fn save(owner: isize, name: &str) -> Result<Option<PathBuf>, String> {
    #[cfg(windows)]
    {
        windows::choose(owner, Some(name))
    }
    #[cfg(not(windows))]
    {
        let _ = (owner, name);
        Err("Native theme export is available on Windows.".into())
    }
}
