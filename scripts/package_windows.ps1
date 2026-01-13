Param(
    [switch]$NoBuild
)

$ErrorActionPreference = "Stop"

Write-Host "==> Packaging Lightspeed for Windows" -ForegroundColor Cyan

# Determine repo root
$ScriptDir = Split-Path -Parent $MyInvocation.MyCommand.Definition
$RepoRoot = Resolve-Path (Join-Path $ScriptDir '..')
Set-Location $RepoRoot

# Verify cargo
if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
    Write-Error "Cargo is required. Install Rust from https://rustup.rs/"
}

if (-not $NoBuild) {
    Write-Host "==> Building release (MSVC toolchain)" -ForegroundColor Green
    cargo build --release
}

$TargetExe = Join-Path $RepoRoot 'target\release\lightspeed.exe'
if (-not (Test-Path $TargetExe)) {
    Write-Error "Release binary not found at $TargetExe. Did the build succeed?"
}

# Staging directory
$DistDir = Join-Path $RepoRoot 'dist'
$AppDir = Join-Path $DistDir 'Lightspeed'
if (Test-Path $AppDir) { Remove-Item $AppDir -Recurse -Force }
New-Item -ItemType Directory -Force -Path $AppDir | Out-Null

# Copy artifacts
Copy-Item $TargetExe (Join-Path $AppDir 'Lightspeed.exe')
Copy-Item (Join-Path $RepoRoot 'LICENSE') $AppDir -ErrorAction SilentlyContinue
Copy-Item (Join-Path $RepoRoot 'README.md') $AppDir -ErrorAction SilentlyContinue

# Create zip
$ZipPath = Join-Path $DistDir 'Lightspeed_Windows.zip'
if (Test-Path $ZipPath) { Remove-Item $ZipPath -Force }
Add-Type -AssemblyName System.IO.Compression.FileSystem
[System.IO.Compression.ZipFile]::CreateFromDirectory($AppDir, $ZipPath)

Write-Host "==> Done" -ForegroundColor Green
Write-Host "Packaged ZIP: $ZipPath"

Write-Host "
How to share:
  - Send Lightspeed_Windows.zip to your friend
  - They should extract it and run Lightspeed.exe
" -ForegroundColor Yellow

