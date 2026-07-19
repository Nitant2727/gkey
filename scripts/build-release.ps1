<#
.SYNOPSIS
  Build optimized release binaries and assemble a dist\ folder.
.DESCRIPTION
  Requires Smart App Control to be OFF — the release profile compiles fresh
  build-script binaries that SAC blocks from executing. Debug binaries work too
  (see the repo target\debug) but release is smaller and faster.
#>
$ErrorActionPreference = 'Stop'
$root = Split-Path $PSScriptRoot -Parent
Push-Location $root
try {
    cargo build --release
    if ($LASTEXITCODE -ne 0) { throw "cargo build failed (is Smart App Control off?)" }

    $dist = Join-Path $root 'dist'
    New-Item -ItemType Directory -Force $dist | Out-Null
    foreach ($f in 'gkeyd.exe', 'gkey-settings.exe', 'gkey-watcher.exe') {
        Copy-Item (Join-Path $root "target\release\$f") $dist -Force
    }
    Copy-Item (Join-Path $root 'config.example.toml') (Join-Path $dist 'gkey.config.example.toml') -Force
    Copy-Item (Join-Path $root 'RUNNING.md') $dist -Force

    Write-Host "`ndist\ ready:" -ForegroundColor Cyan
    Get-ChildItem $dist | Select-Object Name, @{n='KB';e={[math]::Round($_.Length/1KB)}} | Format-Table -Auto
    Write-Host "Run: dist\gkeyd.exe   (then dist\gkey-settings.exe)" -ForegroundColor Green
}
finally { Pop-Location }
