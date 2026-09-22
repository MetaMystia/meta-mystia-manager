#!/usr/bin/env pwsh

<#
.SYNOPSIS
    Build meta-mystia-manager for Windows.
.DESCRIPTION
    Builds the release binary for x86_64-pc-windows-msvc, the only target this
    tool ships.
    On Windows hosts, the native MSVC toolchain is used.
    On non-Windows hosts (macOS/Linux), cargo-xwin is used for cross-compilation.

    Note: On non-Windows hosts, thunk-rs (VC-LTL5 + YY-Thunks) and winres are
    skipped because thunk-rs needs 7z to unpack its archives and its delay-load
    symbols conflict with the lld-link linker used by cargo-xwin, while winres
    needs rc.exe. Since VC-LTL5 is gone as well, the cross-compiled exe links the
    CRT statically, so it is self-contained and needs no VC++ Redistributable,
    but it requires Windows 10+; native Windows builds support Windows 7+ via
    YY-Thunks API polyfills.

    The artifact is collected into the ./target/output directory, named after
    the release convention of the version API: meta-mystia-manager-v{version}.exe
#>

$ErrorActionPreference = "Stop"

$BinName = "meta-mystia-manager"
$Triple = "x86_64-pc-windows-msvc"
$OutputDir = Join-Path $PSScriptRoot "target" "output"

# Detect host OS and choose the build tool
# Both paths target MSVC; on non-Windows hosts cargo-xwin provides the SDK/CRT.
if ($IsWindows) {
  $BuildTool = "cargo"      # native MSVC toolchain
}
else {
  $BuildTool = "xwin"       # cargo-xwin cross-compilation
}

# The artifact name follows the release convention of the version API
# (meta-mystia-manager-v{version}.exe), so read the version from Cargo.toml.
$metadata = cargo metadata --no-deps --format-version 1 --manifest-path (Join-Path $PSScriptRoot "Cargo.toml") | ConvertFrom-Json
$Version = ($metadata.packages | Where-Object { $_.name -eq $BinName }).version
if (-not $Version) {
  Write-Error "Failed to read the version of $BinName from Cargo.toml"
  exit 1
}

Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host " Building: $Triple" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

# Build arguments
$buildArgs = @("--release", "--target", $Triple)

switch ($BuildTool) {
  "xwin" {
    # Cross-compiled exes skip thunk-rs, so VC-LTL5 is not used either and the CRT
    # would be linked dynamically (requiring VCRUNTIME140.dll on the target machine).
    # Link it statically instead to keep the artifact self-contained.
    # LNK4099 is ignored because xwin's static CRT libraries ship without their PDBs.
    $crtFlags = "-C target-feature=+crt-static -C link-arg=/IGNORE:4099"
    $env:RUSTFLAGS = if ($env:RUSTFLAGS) { "$env:RUSTFLAGS $crtFlags" } else { $crtFlags }

    Write-Host "cargo xwin build $($buildArgs -join ' ')" -ForegroundColor Yellow
    & cargo xwin build @buildArgs
  }
  default {
    Write-Host "cargo build $($buildArgs -join ' ')" -ForegroundColor Yellow
    & cargo build @buildArgs
  }
}

if ($LASTEXITCODE -ne 0) {
  Write-Error "Build failed: $Triple"
  exit 1
}

# Collect the artifact
New-Item -ItemType Directory -Path $OutputDir -Force | Out-Null
$srcExe = Join-Path $PSScriptRoot "target" $Triple "release" "$BinName.exe"
$outExe = Join-Path $OutputDir "$BinName-v$Version.exe"

Write-Host "  -> $outExe"
Copy-Item -Path $srcExe -Destination $outExe -Force

Write-Host "`n========================================" -ForegroundColor Green
Write-Host " Build completed successfully!" -ForegroundColor Green
Write-Host "========================================" -ForegroundColor Green
Write-Host "`nOutput files:"
Get-ChildItem $OutputDir -Recurse -File | ForEach-Object {
  $relative = $_.FullName.Substring($OutputDir.Length + 1)
  $sizeKB = [math]::Round($_.Length / 1KB, 1)
  Write-Host "  $relative  ($sizeKB KB)"
}
