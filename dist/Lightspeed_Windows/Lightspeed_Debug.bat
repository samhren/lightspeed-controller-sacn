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
