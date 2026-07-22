//! Embed the tray/app icon as a Windows resource (id 1), the exe icon, and a
//! manifest requesting UIAccess.
//!
//! `uiAccess="true"` lets the overlay draw above the shell's immersive z-band
//! (Start menu, Search, Action Center) — the same privilege the on-screen
//! keyboard and Magnifier use. Windows only grants it when the exe is signed
//! with a machine-trusted certificate AND runs from a secure location
//! (Program Files); see scripts/install.ps1. Anywhere else the exe refuses to
//! start, so the manifest is only embedded when GKEY_UIACCESS=1 is set.

const MANIFEST: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<assembly xmlns="urn:schemas-microsoft-com:asm.v1" manifestVersion="1.0">
  <trustInfo xmlns="urn:schemas-microsoft-com:asm.v3">
    <security>
      <requestedPrivileges>
        <requestedExecutionLevel level="asInvoker" uiAccess="true"/>
      </requestedPrivileges>
    </security>
  </trustInfo>
</assembly>
"#;

fn main() {
    println!("cargo:rerun-if-changed=gkey.ico");
    println!("cargo:rerun-if-env-changed=GKEY_UIACCESS");
    let mut res = winresource::WindowsResource::new();
    res.set_icon_with_id("gkey.ico", "1");
    // Opt in via GKEY_UIACCESS=1: a uiAccess exe that is unsigned or started
    // outside Program Files fails to launch entirely (error 740/1220-class),
    // which would break plain `cargo run` development flows.
    if std::env::var("GKEY_UIACCESS").is_ok_and(|v| v == "1") {
        res.set_manifest(MANIFEST);
    }
    if let Err(e) = res.compile() {
        // Not fatal — the tray falls back to a stock icon if this fails.
        println!("cargo:warning=icon resource embed failed: {e}");
    }
}
