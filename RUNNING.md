# Running gkey on a Smart App Control machine

Windows 11's **Smart App Control (SAC)** blocks unsigned / low-reputation
executables. It's what stops `gkeyd.exe` from launching on this machine. Facts
(verified against Microsoft docs, 2026):

- SAC only trusts apps that are **signed by a CA in Microsoft's Trusted Root
  Program** (or that earn cloud reputation from wide download volume).
  Self-signed certificates do **not** work for SAC.
- A valid **RSA** signature from a trusted provider makes SAC allow the app
  immediately — no reputation wait. (ECC signatures are not supported by SAC.)
- **Turning SAC off is reversible** on current Windows 11 (recent updates,
  incl. the April 2026 KB5083769, let admins toggle it off and back on without a
  clean reinstall).

You don't need a *release* build — the debug binaries are fully functional. The
only obstacle is SAC allowing the (unsigned) exe to launch.

## Path A — Toggle SAC off (free, immediate; recommended for personal use)

Best if you just want to run your own tool. It's now reversible.

1. **Settings → Privacy & security → Windows Security → App & browser control →
   Smart App Control settings → Off.**
2. Run `gkeyd.exe` (and `gkey-settings.exe`). They launch normally now.
3. Optional: keep `gkey-watcher.exe` in the same folder.

Trade-off: while SAC is off you lose that protection layer for *all* apps. You
can turn it back on later — but then gkey is blocked again unless it's signed
(Path B). Many people run a dev box with SAC off and reserve SAC for locked-down
machines.

## Path B — Sign with a trusted certificate (run with SAC on)

Best if you want SAC to stay enabled. Two sub-options:

### B1. Azure Trusted Signing / Artifact Signing — cheapest legit path (~$9.99/mo)

Microsoft-managed CA; certificates chain to a trusted root, so SAC accepts them.

1. Create an Azure subscription; add the **Trusted Signing** (Artifact Signing)
   resource; complete **identity validation** (individual validation is
   available; takes a few days).
2. Install the Trusted Signing client + the signing dlib.
3. Sign the binaries (see `scripts/sign.ps1 -Azure ...`), then run with SAC on.

Note: Trusted Signing clears **SAC** immediately (trusted signature), but
**SmartScreen** reputation still accrues over download volume — irrelevant for
running your own build locally.

### B2. OV/EV code-signing certificate from a CA (DigiCert, Sectigo, …)

$200–500/yr, hardware token or cloud HSM. Overkill for personal use; same end
result. Export/reference the cert and use `scripts/sign.ps1 -PfxPath ...`.

## Signing the binaries

Once you have a trusted cert, `scripts/sign.ps1` signs all three exes with an
RSA + SHA-256 + RFC-3161 timestamp (the shape SAC wants). See that script's
header for the exact invocation for a PFX or for Azure Trusted Signing.

## UIAccess install (overlay above Start/Search)

The shell's Start menu, Search, and Action Center flyouts live in a z-band
above every normal topmost window, so hint labels drawn over them are
invisible — unless the daemon has **UIAccess** (the privilege the On-Screen
Keyboard and Magnifier use). Windows grants it only when the exe (1) requests
it in its manifest, (2) is signed with a machine-trusted certificate, and
(3) runs from Program Files.

One-time setup, from an **elevated** PowerShell:

```powershell
cd <repo>
$env:GKEY_UIACCESS = '1'
cargo build --release
scripts\install.ps1
```

The script creates a local self-signed code-signing cert (first run only),
trusts it (LocalMachine Root + TrustedPublisher — local key, nothing external),
signs the binaries, installs to `C:\Program Files\gkey\`, and restarts the
daemon de-elevated. Verify with the log line `UIAccess: true`.

Notes:
- A `GKEY_UIACCESS=1` build refuses to start outside Program Files
  ("A referral was returned from the server") — that's the OS enforcing
  UIAccess rules, not a bug. Build without the env var for `cargo run` dev.
- This is separate from SAC: SAC ignores self-signed certs, so with SAC ON
  you'd still need path B for the *SAC* half. With SAC off (path A), the
  self-signed cert is enough for UIAccess.
- Remove everything: `scripts\install.ps1 -Uninstall`.

## What does NOT work

- Self-signed certs (even added to Trusted Root / Trusted Publishers) — SAC uses
  its own trust list, not the machine cert stores.
- Renaming, unblocking (`Unblock-File`), or Mark-of-the-Web removal — SAC is
  code-integrity based, not MOTW based.
