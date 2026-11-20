# SpiderNotes — one-shot Windows build (NSIS + MSI installers)
# Run from a PowerShell in the repo root (or the app/ dir's parent).
#
# Prerequisites (auto-installed by this script if missing):
#   - Rust (rustup)
#   - Node.js >= 20 (with pnpm via corepack)
#   - Visual Studio Build Tools "Desktop development with C++" (linker)
#     https://visualstudio.microsoft.com/visual-cpp-build-tools/

$ErrorActionPreference = "Stop"
Set-Location $PSScriptRoot\..

Write-Host "==> Checking rust" -ForegroundColor Cyan
if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
    Write-Host "Installing Rust via rustup..."
    Invoke-WebRequest -Uri "https://static.rust-lang.org/rustup/dist/x86_64-pc-windows-msvc/rustup-init.exe" -OutFile "$env:TEMP\rustup-init.exe"
    & "$env:TEMP\rustup-init.exe" -y --default-toolchain stable
    $env:Path = "$env:USERPROFILE\.cargo\bin;$env:Path"
}
cargo --version

Write-Host "==> Checking node" -ForegroundColor Cyan
if (-not (Get-Command node -ErrorAction SilentlyContinue)) {
    throw "Node.js not found. Install from https://nodejs.org (LTS) and re-run."
}
node --version

Write-Host "==> pnpm install" -ForegroundColor Cyan
Push-Location app
corepack enable
corepack pnpm install --frozen-lockfile

Write-Host "==> cargo tauri build (msi + nsis)" -ForegroundColor Cyan
corepack pnpm tauri build --bundles msi,nsis

Write-Host "==> Done" -ForegroundColor Green
Get-ChildItem src-tauri\target\release\bundle\nsis\*.exe | Format-Table Name, Length
Get-ChildItem src-tauri\target\release\bundle\msi\*.msi | Format-Table Name, Length
Pop-Location
