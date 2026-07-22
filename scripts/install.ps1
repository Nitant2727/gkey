<#
.SYNOPSIS
  Install gkey to Program Files with UIAccess so the hint overlay can draw
  above the Start menu and Search flyouts.

.DESCRIPTION
  Windows grants a process UIAccess only when ALL of these hold:
    1. Its manifest requests uiAccess="true"  (build with GKEY_UIACCESS=1)
    2. The exe is Authenticode-signed with a certificate the MACHINE trusts
    3. It runs from a secure location (Program Files / System32)

  This script (run elevated):
    - creates a local self-signed code-signing certificate on first run and
      trusts it (LocalMachine Root + TrustedPublisher)
    - signs the release binaries
    - copies them to  C:\Program Files\gkey\
    - restarts the daemon from there, de-elevated via explorer.exe

  SECURITY NOTE: this adds a locally-generated certificate to this machine's
  trusted roots. The private key stays in your user store; nothing external
  is trusted. Delete with:  scripts\install.ps1 -Uninstall

.NOTES
  Build first:  $env:GKEY_UIACCESS='1'; cargo build --release
#>
[CmdletBinding()]
param(
    [string] $SourceDir = "$PSScriptRoot\..\target\release",
    [switch] $Uninstall
)

$ErrorActionPreference = 'Stop'
$InstallDir = Join-Path $env:ProgramFiles 'gkey'
$CertSubject = 'CN=gkey local code signing'

$isAdmin = ([Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()
    ).IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
if (-not $isAdmin) {
    Write-Error 'Run elevated (right-click PowerShell -> Run as administrator).'
}

function Stop-Gkey {
    foreach ($n in 'gkeyd', 'gkey-watcher', 'gkey-settings') {
        try { Get-Process $n -ErrorAction Stop | Stop-Process -Force -Confirm:$false } catch {}
    }
    Start-Sleep -Milliseconds 300
}

if ($Uninstall) {
    Stop-Gkey
    if (Test-Path $InstallDir) { Remove-Item -Recurse -Force $InstallDir }
    foreach ($store in 'Root', 'TrustedPublisher') {
        Get-ChildItem "Cert:\LocalMachine\$store" |
            Where-Object { $_.Subject -eq $CertSubject } | Remove-Item
    }
    Get-ChildItem 'Cert:\CurrentUser\My' |
        Where-Object { $_.Subject -eq $CertSubject } | Remove-Item
    Write-Host 'gkey uninstalled; certificate removed from trust stores.'
    return
}

# --- certificate -------------------------------------------------------------
$cert = Get-ChildItem 'Cert:\CurrentUser\My' |
    Where-Object { $_.Subject -eq $CertSubject -and $_.NotAfter -gt (Get-Date) } |
    Select-Object -First 1
if (-not $cert) {
    Write-Host 'Creating local code-signing certificate...'
    $cert = New-SelfSignedCertificate -Type CodeSigningCert -Subject $CertSubject `
        -CertStoreLocation 'Cert:\CurrentUser\My' -NotAfter (Get-Date).AddYears(10) `
        -KeyAlgorithm RSA -KeyLength 3072
}

$cerPath = Join-Path $env:TEMP 'gkey-signing.cer'
Export-Certificate -Cert $cert -FilePath $cerPath | Out-Null
foreach ($store in 'Root', 'TrustedPublisher') {
    $already = Get-ChildItem "Cert:\LocalMachine\$store" |
        Where-Object { $_.Thumbprint -eq $cert.Thumbprint }
    if (-not $already) {
        Import-Certificate -FilePath $cerPath -CertStoreLocation "Cert:\LocalMachine\$store" | Out-Null
        Write-Host "Trusted certificate in LocalMachine\$store."
    }
}
Remove-Item $cerPath -Force

# --- sign --------------------------------------------------------------------
$exes = @('gkeyd.exe', 'gkey-settings.exe', 'gkey-watcher.exe') |
    ForEach-Object { Join-Path $SourceDir $_ } | Where-Object { Test-Path $_ }
if (-not $exes) { Write-Error "No binaries in $SourceDir — build first." }
foreach ($exe in $exes) {
    $r = Set-AuthenticodeSignature -FilePath $exe -Certificate $cert -HashAlgorithm SHA256
    if ($r.Status -ne 'Valid') { Write-Error "Signing failed for ${exe}: $($r.StatusMessage)" }
    Write-Host "Signed $(Split-Path -Leaf $exe)."
}

# --- install -----------------------------------------------------------------
Stop-Gkey
New-Item -ItemType Directory -Force $InstallDir | Out-Null
foreach ($exe in $exes) { Copy-Item $exe $InstallDir -Force }
$example = Join-Path $PSScriptRoot '..\config.example.toml'
if (Test-Path $example) { Copy-Item $example (Join-Path $InstallDir 'config.example.toml') -Force }
Write-Host "Installed to $InstallDir."

# --- start de-elevated -------------------------------------------------------
# Launching directly from this elevated shell would run the daemon as admin;
# explorer.exe re-parents the launch into the normal user context.
explorer.exe (Join-Path $InstallDir 'gkeyd.exe')
Write-Host 'Daemon started. Check the log for "UIAccess: true".'
