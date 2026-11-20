fn main() {
    println!("cargo:rerun-if-changed=../../assets/icon.ico");
    println!("cargo:rerun-if-changed=build.rs");
    #[cfg(windows)]
    embed_resources();
}

#[cfg(windows)]
fn embed_resources() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let version = std::env::var("CARGO_PKG_VERSION").unwrap();
    let mut res = winresource::WindowsResource::new();
    res.set_icon("../../assets/icon.ico")
        .set("ProductName", "Glossplay")
        .set("FileDescription", "Glossplay")
        .set("CompanyName", "Glossplay")
        .set("LegalCopyright", "GPL-3.0-or-later")
        .set("ProductVersion", &version)
        .set("FileVersion", &version);
    // I fail loudly: an exe silently built without its icon is worse than a broken build.
    if let Err(e) = res.compile() {
        panic!(
            "could not embed the icon and version info (needs windres for GNU or rc.exe from the Windows SDK for MSVC): {e}"
        );
    }
}
