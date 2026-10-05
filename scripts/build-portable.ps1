# Builds single-file COFDMtv executables for 64-bit Windows 10/11 that need nothing
# installed: the C runtime is linked statically (no Visual C++ redistributable). The
# results go to exe\ (git-ignored): cofdmtv-gui.exe (receiver + transmitter) and
# cofdmtv.exe (command line).
#
#   powershell -ExecutionPolicy Bypass -File scripts\build-portable.ps1
#
# Builds in target\portable, so the usual target directory and its cache stay as they
# are. .github/workflows/release.yml builds the same way for a release.

$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $PSScriptRoot
$triple = "x86_64-pc-windows-msvc"

# With an explicit --target the flags reach only the executables (and the C code that cc
# builds for them, libwebp), not build scripts and proc macros.
$env:RUSTFLAGS = "-C target-feature=+crt-static"
$env:CARGO_TARGET_DIR = Join-Path $root "target\portable"

Push-Location $root
try {
    cargo build --release --target $triple -p cofdmtv-gui -p cofdmtv-cli
    if ($LASTEXITCODE -ne 0) { throw "cargo build failed ($LASTEXITCODE)" }
    $out = Join-Path $root "exe"
    New-Item -ItemType Directory -Force $out | Out-Null
    foreach ($exe in "cofdmtv-gui.exe", "cofdmtv.exe") {
        $built = Join-Path $env:CARGO_TARGET_DIR "$triple\release\$exe"
        if (Select-String -Path $built -Pattern "vcruntime140", "api-ms-win-crt" -SimpleMatch -Quiet) {
            throw "$exe imports a C runtime DLL: not a static build"
        }
        Copy-Item $built $out -Force
        $mb = (Get-Item (Join-Path $out $exe)).Length / 1MB
        Write-Host ("{0,-16} {1,6:N1} MB" -f $exe, $mb)
    }
    Write-Host "written to $out"
} finally {
    Pop-Location
}
