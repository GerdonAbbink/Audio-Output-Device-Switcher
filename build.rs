//! Embeds the Windows version information and app icon into the exe.
//! FileVersion and ProductVersion come from the version in Cargo.toml.

const PRODUCT_NAME: &str = "Audio Output Device Switcher";

fn main() {
    println!("cargo:rerun-if-changed=assets/app.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let mut res = winresource::WindowsResource::new();
    if std::path::Path::new("assets/app.ico").exists() {
        res.set_icon("assets/app.ico");
    } else {
        // Only while bootstrapping: `cargo run --example make_icon` creates it.
        println!("cargo:warning=assets/app.ico missing; building without an app icon");
    }
    res.set("ProductName", PRODUCT_NAME)
        .set("FileDescription", PRODUCT_NAME)
        .set("CompanyName", "Gerdon Abbink")
        .set("LegalCopyright", "Copyright (c) 2026 Gerdon Abbink. MIT License.")
        .set("OriginalFilename", "audio-output-device-switcher.exe")
        .set("InternalName", "audio-output-device-switcher");
    res.compile().expect("embed Windows resources");
}
