fn main() {
    // Version info in clipdeck_micfx.dll (product name and version, required for code signing).
    #[cfg(windows)]
    {
        if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
            let mut res = winresource::WindowsResource::new();
            res.set("ProductName", "Clipdeck");
            res.set("FileDescription", "Clipdeck in-microphone effect");
            res.set("LegalCopyright", "GPL-3.0-or-later");
            if let Err(e) = res.compile() {
                println!("cargo:warning=could not embed Windows resources: {e}");
            }
        }
    }
}
