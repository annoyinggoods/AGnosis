# Build agnosis-setup.exe: the release binary, then the Inno Setup script.
#
#   powershell -ExecutionPolicy Bypass -File packaging/windows/build-installer.ps1
#
# Reads the version from the root Cargo.toml and hands it to ISCC as
# /DAppVersion, so the installer and the binary never disagree. Inno Setup 6
# is found in the per-user or the machine-wide install location.

$ErrorActionPreference = "Stop"
$repoRoot = (Resolve-Path (Join-Path $PSScriptRoot "..\..")).Path

Push-Location $repoRoot
try {
    Write-Host "==> cargo build --release"
    cargo build --release
    if ($LASTEXITCODE -ne 0) { throw "cargo build --release failed" }

    $isccCandidates = @(
        (Join-Path $env:LOCALAPPDATA "Programs\Inno Setup 6\ISCC.exe"),
        "${env:ProgramFiles(x86)}\Inno Setup 6\ISCC.exe",
        "${env:ProgramFiles}\Inno Setup 6\ISCC.exe"
    )
    $iscc = $isccCandidates | Where-Object { Test-Path $_ } | Select-Object -First 1
    if (-not $iscc) { throw "ISCC.exe not found: install Inno Setup 6 (winget install JRSoftware.InnoSetup)" }

    $versionLine = Select-String -Path (Join-Path $repoRoot "Cargo.toml") -Pattern '^version = "(.+)"' | Select-Object -First 1
    $version = $versionLine.Matches[0].Groups[1].Value
    Write-Host "==> ISCC /DAppVersion=$version"
    & $iscc "/DAppVersion=$version" (Join-Path $PSScriptRoot "agnosis.iss")
    if ($LASTEXITCODE -ne 0) { throw "ISCC failed" }
    Write-Host "==> packaging/windows/dist/agnosis-setup.exe ready"
}
finally {
    Pop-Location
}
