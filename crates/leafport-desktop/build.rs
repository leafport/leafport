fn main() {
    #[cfg(windows)]
    embed_windows_resources();
}

/// Windows shows an executable's embedded icon in Explorer, shortcuts and the taskbar.
#[cfg(windows)]
fn embed_windows_resources() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    println!("cargo::rerun-if-changed=data/icons/leafport.ico");
    winresource::WindowsResource::new()
        .set_icon("data/icons/leafport.ico")
        .compile()
        .expect("embedding the Windows resources");
}
