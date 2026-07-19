//! Embed the tray/app icon as a Windows resource (id 1) and the exe icon.

fn main() {
    println!("cargo:rerun-if-changed=gkey.ico");
    let mut res = winresource::WindowsResource::new();
    res.set_icon_with_id("gkey.ico", "1");
    if let Err(e) = res.compile() {
        // Not fatal — the tray falls back to a stock icon if this fails.
        println!("cargo:warning=icon resource embed failed: {e}");
    }
}
