//! Embed the shared gkey icon as resource id 1 (and the exe icon).

fn main() {
    let icon = "../gkeyd/gkey.ico";
    println!("cargo:rerun-if-changed={icon}");
    let mut res = winresource::WindowsResource::new();
    res.set_icon_with_id(icon, "1");
    if let Err(e) = res.compile() {
        println!("cargo:warning=icon resource embed failed: {e}");
    }
}
