fn main() {
    println!("cargo:rerun-if-changed=../../assets/brand/foldex.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let icon =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/brand/foldex.ico");
        winresource::WindowsResource::new()
            .set_icon(icon.to_str().expect("Foldex icon path must be UTF-8"))
            .set("ProductName", "Foldex")
            .set("FileDescription", "Foldex - native Windows file explorer")
            .set("InternalName", "Foldex")
            .set("OriginalFilename", "Foldex.exe")
            .compile()
            .expect("Compile Foldex Windows icon and version resources");
    }
}
