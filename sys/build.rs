// Compiles the AppKit shims in mac/ (the menu bar item, the download panel and
// Finder's services) on macOS. Elsewhere there is nothing to build, and the
// functions in src/ that call them do nothing.
fn main() {
    println!("cargo:rerun-if-changed=mac");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("macos") {
        return;
    }
    cc::Build::new()
        .files(["mac/tray_darwin.m", "mac/quick_darwin.m", "mac/services_darwin.m"])
        .flag("-fobjc-arc")
        .flag("-Wno-unused-parameter")
        .include("mac")
        .compile("fio_mac");
    println!("cargo:rustc-link-lib=framework=Cocoa");
}
