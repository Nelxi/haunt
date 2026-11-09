// Puts the ghost icon into haunt.exe, so Explorer, shortcuts and pinned taskbar
// buttons show it too (the window icon alone does not cover those).
fn main() {
    println!("cargo:rerun-if-changed=assets/icon.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("assets/icon.ico");
        if let Err(e) = res.compile() {
            // A missing resource compiler should not block the build; the window icon still works.
            println!("cargo:warning=could not embed the exe icon: {e}");
        }
    }
}
