<#
.SYNOPSIS
  Code-sign the gkey binaries so Smart App Control allows them.

.DESCRIPTION
  Signs with RSA + SHA-256 + an RFC-3161 timestamp — the shape Smart App Control
  requires. Point it at a trusted certificate one of three ways:

    # 1) A PFX exported from an OV/EV certificate
    scripts\sign.ps1 -PfxPath C:\certs\gkey.pfx -PfxPassword (Read-Host -AsSecureString)

    # 2) A cert already in your certificate store, by thumbprint
    scripts\sign.ps1 -Thumbprint 1A2B3C...

    # 3) Azure Trusted Signing / Artifact Signing (Microsoft-managed CA)
    #    Needs the Trusted Signing dlib + a metadata json (endpoint, account,
    #    cert-profile). This is the cheapest path that satisfies SAC.
    scripts\sign.ps1 -AzureDlib "C:\path\Azure.CodeSigning.Dlib.dll" -AzureMetadata "C:\path\metadata.json"

  Self-signed certificates do NOT satisfy Smart App Control — see RUNNING.md.

.NOTES
  Defaults to the debug binaries (they are fully functional; a release build is
  not required). Pass -Dir to sign a different folder.
#>
[CmdletBinding(DefaultParameterSetName = 'Pfx')]
param(
    [Parameter(ParameterSetName = 'Pfx')]      [string] $PfxPath,
    [Parameter(ParameterSetName = 'Pfx')]      [System.Security.SecureString] $PfxPassword,
    [Parameter(ParameterSetName = 'Store')]    [string] $Thumbprint,
    [Parameter(ParameterSetName = 'Azure')]    [string] $AzureDlib,
    [Parameter(ParameterSetName = 'Azure')]    [string] $AzureMetadata,
    [string] $Dir = "$PSScriptRoot\..\target\debug",
    [string] $Timestamp = 'http://timestamp.digicert.com'
)

$ErrorActionPreference = 'Stop'

function Find-SignTool {
    $cmd = Get-Command signtool.exe -ErrorAction SilentlyContinue
    if ($cmd) { return $cmd.Source }
    $hit = Get-ChildItem 'C:\Program Files (x86)\Windows Kits\10\bin' -Recurse -Filter signtool.exe -ErrorAction SilentlyContinue |
        Where-Object { $_.FullName -match '\\x64\\' } |
        Sort-Object FullName -Descending | Select-Object -First 1
    if ($hit) { return $hit.FullName }
    throw 'signtool.exe not found. Install the Windows SDK.'
}

$signtool = Find-SignTool
$exes = 'gkeyd.exe', 'gkey-settings.exe', 'gkey-watcher.exe' |
    ForEach-Object { Join-Path $Dir $_ } |
    Where-Object { Test-Path $_ }
if (-not $exes) { throw "No gkey binaries found in $Dir. Build first (cargo build)." }

switch ($PSCmdlet.ParameterSetName) {
    'Pfx' {
        if (-not $PfxPath) { throw 'Provide -PfxPath (or use -Thumbprint / -AzureDlib).' }
        $plain = if ($PfxPassword) {
            [Runtime.InteropServices.Marshal]::PtrToStringAuto(
                [Runtime.InteropServices.Marshal]::SecureStringToBSTR($PfxPassword))
        } else { $null }
        foreach ($exe in $exes) {
            $args = @('sign', '/fd', 'SHA256', '/tr', $Timestamp, '/td', 'SHA256', '/f', $PfxPath)
            if ($plain) { $args += @('/p', $plain) }
            $args += $exe
            & $signtool @args
        }
    }
    'Store' {
        foreach ($exe in $exes) {
            & $signtool sign /fd SHA256 /tr $Timestamp /td SHA256 /sha1 $Thumbprint $exe
        }
    }
    'Azure' {
        if (-not (Test-Path $AzureDlib) -or -not (Test-Path $AzureMetadata)) {
            throw 'Provide valid -AzureDlib and -AzureMetadata (Trusted Signing).'
        }
        foreach ($exe in $exes) {
            & $signtool sign /v /fd SHA256 /tr $Timestamp /td SHA256 `
                /dlib $AzureDlib /dmdf $AzureMetadata $exe
        }
    }
}

Write-Host "`nVerifying signatures:" -ForegroundColor Cyan
foreach ($exe in $exes) { & $signtool verify /pa /v $exe }
