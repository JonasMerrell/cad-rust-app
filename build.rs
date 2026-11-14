// Embeds the app icon (and version info) into rcad.exe on Windows. Nothing to do elsewhere:
// Linux gets its icon at runtime (main.rs) and from packaging/linux/rcad.desktop.
fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=assets/icon/rcad.ico");
    #[cfg(windows)]
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("assets/icon/rcad.ico");
        res.set("ProductName", "rCAD");
        res.set("FileDescription", "rCAD");
        if let Err(e) = res.compile() {
            // a missing resource compiler shouldn't stop the build, the exe just has no icon
            println!("cargo:warning=could not embed the Windows icon: {e}");
        }
    }
}
