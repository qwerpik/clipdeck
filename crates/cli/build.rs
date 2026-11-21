fn main() {
    // Embed the icon and version info into clipdeck.exe.
    #[cfg(windows)]
    {
        if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
            let mut res = winresource::WindowsResource::new();
            res.set_icon("../../assets/clipdeck.ico");
            res.set("ProductName", "Clipdeck");
            res.set("FileDescription", "Clipdeck soundboard");
            res.set("LegalCopyright", "GPL-3.0-or-later");
            if let Err(e) = res.compile() {
                println!("cargo:warning=could not embed Windows resources: {e}");
            }
        }
    }
}
