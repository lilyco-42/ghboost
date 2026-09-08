@echo off
REM Double-click wrapper for install.ps1.
REM Windows opens .ps1 in Notepad by default when double-clicked, and the
REM default execution policy blocks scripts - so a "fool-proof" installer
REM has to be something that actually runs on a double click.
REM ASCII only: cmd.exe uses the OEM codepage, non-ASCII here turns to mojibake.
powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0install.ps1" %*
echo.
pause
