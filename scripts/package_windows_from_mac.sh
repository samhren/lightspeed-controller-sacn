#!/usr/bin/env bash
set -euo pipefail

echo "==> Packaging Lightspeed for Windows (from macOS)"

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$REPO_ROOT"

if ! command -v rustup >/dev/null 2>&1; then
  echo "error: rustup is required. Install Rust from https://rustup.rs/" >&2
  exit 1
fi

if ! command -v cargo >/dev/null 2>&1; then
  echo "error: cargo not found on PATH" >&2
  exit 1
fi

TARGET_TRIPLE="x86_64-pc-windows-gnu"

echo "==> Ensuring Rust target $TARGET_TRIPLE is installed"
rustup target add "$TARGET_TRIPLE" >/dev/null 2>&1 || true

if ! command -v x86_64-w64-mingw32-gcc >/dev/null 2>&1; then
  echo "==> Installing hint: Homebrew mingw-w64 toolchain is required for cross-compilation" >&2
  echo "   Run: brew install mingw-w64" >&2
  exit 1
fi

echo "==> Building release for $TARGET_TRIPLE (static runtime)"

# Set cross-compilation environment for C/C++ dependencies
export CC_x86_64_pc_windows_gnu="x86_64-w64-mingw32-gcc"
export CXX_x86_64_pc_windows_gnu="x86_64-w64-mingw32-g++"
export AR_x86_64_pc_windows_gnu="x86_64-w64-mingw32-ar"
export CXXFLAGS="-static-libstdc++ -static-libgcc"

# Create workaround for rusty_link's -lc++ (macOS-ism, should be -lstdc++ on mingw)
# We create a symlink from libc++.a to libstdc++.a
MINGW_LIB_DIR=$(x86_64-w64-mingw32-gcc -print-file-name=libstdc++.a 2>/dev/null | xargs dirname)
if [ -n "$MINGW_LIB_DIR" ] && [ -f "$MINGW_LIB_DIR/libstdc++.a" ] && [ ! -f "$MINGW_LIB_DIR/libc++.a" ]; then
    echo "   Creating libc++.a symlink to fix rusty_link cross-compilation..."
    ln -sf libstdc++.a "$MINGW_LIB_DIR/libc++.a" 2>/dev/null || true
fi

# Ensure static libstdc++/libgcc are used (also configured in .cargo/config.toml)
export RUSTFLAGS="${RUSTFLAGS:-} -C link-arg=-static -C link-arg=-static-libgcc -C link-arg=-static-libstdc++"

cargo build --release --target "$TARGET_TRIPLE"

DIST_DIR="$REPO_ROOT/dist"
APP_DIR="$DIST_DIR/Lightspeed_Windows"
BIN_PATH="$REPO_ROOT/target/$TARGET_TRIPLE/release/lightspeed.exe"

rm -rf "$APP_DIR"
mkdir -p "$APP_DIR"

cp "$BIN_PATH" "$APP_DIR/Lightspeed.exe"
[ -f "$REPO_ROOT/README.md" ] && cp "$REPO_ROOT/README.md" "$APP_DIR/"
[ -f "$REPO_ROOT/LICENSE" ] && cp "$REPO_ROOT/LICENSE" "$APP_DIR/"

# Create debug launcher batch file
cat > "$APP_DIR/Lightspeed_Debug.bat" << 'BATCH'
@echo off
echo ========================================
echo Lightspeed Controller - Debug Mode
echo ========================================
echo.
echo This will show detailed connection logs for:
echo   - MIDI device detection and connection
echo   - sACN/E1.31 light network setup
echo.
echo Press Ctrl+C to stop, or close this window.
echo ========================================
echo.
set RUST_LOG=debug
Lightspeed.exe
pause
BATCH

echo "   + Created Lightspeed_Debug.bat for troubleshooting"

echo "==> Verifying MinGW runtime DLLs (should be unnecessary due to static linking)"
DLL_NAMES=(libstdc++-6.dll libgcc_s_seh-1.dll libwinpthread-1.dll)

BREW_PREFIX="$(brew --prefix 2>/dev/null || true)"

# Derive likely locations from the toolchain itself
LD_PATH="$(x86_64-w64-mingw32-gcc -print-prog-name=ld 2>/dev/null || true)"
LD_DIR="$(dirname "$LD_PATH")"
MINGW_ROOT="$(cd "$LD_DIR/.." 2>/dev/null && pwd || true)"
MINGW_BIN="${MINGW_ROOT:+$MINGW_ROOT/bin}"

# Include gcc search dirs (programs) for completeness
PROGRAM_DIRS=$(x86_64-w64-mingw32-gcc -print-search-dirs 2>/dev/null | awk -F= '/^programs:/ {print $2}' | tr ':' '\n')

SEARCH_DIRS=(
  "$MINGW_BIN"
  $PROGRAM_DIRS
  "$BREW_PREFIX/opt/mingw-w64/toolchain-x86_64/x86_64-w64-mingw32/bin"
  "$BREW_PREFIX/opt/mingw-w64/toolchain-x86_64/bin"
  "$BREW_PREFIX/opt/mingw-w64/bin"
  "/usr/local/opt/mingw-w64/toolchain-x86_64/x86_64-w64-mingw32/bin"
  "/opt/homebrew/opt/mingw-w64/toolchain-x86_64/x86_64-w64-mingw32/bin"
)

MISSING_DLLS=()
for dll in "${DLL_NAMES[@]}"; do
  FOUND_PATH=""
  for dir in "${SEARCH_DIRS[@]}"; do
    [ -z "$dir" ] && continue
    CAND="$dir/$dll"
    if [ -f "$CAND" ]; then
      FOUND_PATH="$CAND"
      break
    fi
  done
  if [ -n "$FOUND_PATH" ]; then
    echo "   + Found $dll -> $(basename "$FOUND_PATH")"
    cp "$FOUND_PATH" "$APP_DIR/"
  else
    echo "   - Not found: $dll" >&2
    MISSING_DLLS+=("$dll")
  fi
done

ZIP_PATH="$DIST_DIR/Lightspeed_Windows.zip"
rm -f "$ZIP_PATH"

echo "==> Creating ZIP at $ZIP_PATH"
(cd "$DIST_DIR" && zip -r -q "$(basename "$ZIP_PATH")" "$(basename "$APP_DIR")")

echo "==> Done"
echo "Packaged: $ZIP_PATH"

if [ ${#MISSING_DLLS[@]} -gt 0 ]; then
  echo "Note: The following runtime DLLs were not auto-located:" >&2
  printf '  - %s\n' "${MISSING_DLLS[@]}" >&2
  echo "Static linking should avoid these, but if Windows reports missing DLLs, locate them within your Homebrew mingw-w64 installation and place next to Lightspeed.exe before zipping." >&2
  echo "Try: find \"$BREW_PREFIX/opt/mingw-w64\" -name 'libstdc++-6.dll' -o -name 'libgcc_s_seh-1.dll' -o -name 'libwinpthread-1.dll'" >&2
fi

echo ""
echo "========================================"
echo "Share instructions:"
echo "  1. Send: dist/Lightspeed_Windows.zip"
echo "  2. Friend extracts the ZIP"
echo "  3. Run Lightspeed.exe (normal mode)"
echo "     OR run Lightspeed_Debug.bat (to see connection logs)"
echo "========================================"
