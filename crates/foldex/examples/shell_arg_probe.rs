//! Headless receiver used only by the isolated shell-registration check.
#![cfg_attr(windows, windows_subsystem = "windows")]
fn main() {
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        let arguments: Vec<Vec<u16>> = std::env::args_os()
            .skip(1)
            .map(|argument| argument.encode_wide().collect())
            .collect();
        let output = std::env::current_exe()
            .unwrap()
            .with_file_name("received-arguments.json");
        std::fs::write(output, serde_json::to_vec(&arguments).unwrap()).unwrap();
    }
}
