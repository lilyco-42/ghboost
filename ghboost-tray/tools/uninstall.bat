@echo off
REM Double-click wrapper for uninstall.ps1 (see install.bat for why this exists).
REM ASCII only: cmd.exe uses the OEM codepage, non-ASCII here turns to mojibake.
powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0uninstall.ps1" %*
echo.
pause
