# ghboost uninstaller (ASCII only - PowerShell 5.1 reads BOM-less UTF-8 as ANSI)
# Reverses everything tools/install.ps1 did, and restores hosts + system proxy.
param(
    [string]$InstallDir = "$env:LOCALAPPDATA\ghboost",
    # Also delete the install dir (kernel binary, mihomo config, rule databases)
    [switch]$RemoveData,
    # Internal: set when the script relaunches itself elevated, to avoid a loop
    [switch]$NoElevate
)

$ErrorActionPreference = "Continue"
$Exe = Join-Path $InstallDir "ghboost-tray.exe"

# --- 0. Elevate: restoring hosts requires writing
#     C:\Windows\System32\drivers\etc\hosts, which needs admin.
#     Self-elevate rather than silently leaving the user's hosts modified:
#     a half-uninstalled accelerator is worse than no uninstaller at all.
$IsAdmin = ([Security.Principal.WindowsPrincipal] `
    [Security.Principal.WindowsIdentity]::GetCurrent()
).IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)

if (-not $IsAdmin -and -not $NoElevate) {
    Write-Host "Requesting administrator rights to restore your hosts file..."
    $Arg = "-NoProfile -ExecutionPolicy Bypass -File `"$PSCommandPath`" -NoElevate"
    if ($RemoveData) { $Arg += " -RemoveData" }
    try {
        Start-Process powershell -ArgumentList $Arg -Verb RunAs
    } catch {
        Write-Host "WARN: elevation declined: $($_.Exception.Message)" -ForegroundColor Yellow
        Write-Host "      You can rerun this script as Administrator later." -ForegroundColor Yellow
    }
    exit
}

# --- 1. Stop the running instance (graceful via --quit, then force)
Write-Host "Stopping ghboost..."
if (Test-Path $Exe) {
    & $Exe --quit 2>$null
    Start-Sleep -Seconds 2
}
Get-Process -Name "ghboost-tray" -ErrorAction SilentlyContinue | ForEach-Object {
    Write-Host "  killing pid $($_.Id)"
    Stop-Process -Id $_.Id -Force
}

# --- 2. Restore what we changed on the system.
#     --restore does two things: turn the system proxy off, and strip the
#     ghboost block out of the hosts file. It prints [OK]/[FAIL] per step.
if (Test-Path $Exe) {
    Write-Host "Restoring hosts and system proxy..."
    & $Exe --restore
    if ($LASTEXITCODE -ne 0) {
        Write-Host "WARN: restore reported a problem (see the lines above)." -ForegroundColor Yellow
    }
} else {
    Write-Host "WARN: $Exe not found - cannot restore hosts automatically." -ForegroundColor Yellow
    Write-Host "      Remove the 'ghboost' block from" -ForegroundColor Yellow
    Write-Host "      C:\Windows\System32\drivers\etc\hosts manually." -ForegroundColor Yellow
}

# --- 3. Remove autostart entry (HKCU Run, set via Set-ItemProperty)
$RunKey = "HKCU:\Software\Microsoft\Windows\CurrentVersion\Run"
if (Get-ItemProperty -Path $RunKey -Name "ghboost" -ErrorAction SilentlyContinue) {
    Remove-ItemProperty -Path $RunKey -Name "ghboost"
    Write-Host "Autostart entry removed."
}

# --- 4. Remove desktop shortcut
$Desktop = [Environment]::GetFolderPath("Desktop")
$Lnk = Join-Path $Desktop "ghboost.lnk"
if (Test-Path $Lnk) {
    Remove-Item $Lnk -Force
    Write-Host "Desktop shortcut removed."
}

# --- 5. Optionally remove installed files
if ($RemoveData) {
    if (Test-Path $InstallDir) {
        Remove-Item $InstallDir -Recurse -Force
        Write-Host "Removed $InstallDir (kernel, config, rule databases)."
    }
} else {
    Write-Host ""
    Write-Host "Kept $InstallDir (kernel + settings)." -ForegroundColor DarkGray
    Write-Host "To delete it too:  .\uninstall.ps1 -RemoveData" -ForegroundColor DarkGray
    Write-Host "To uninstall the program itself: remove $Exe" -ForegroundColor DarkGray
}

Write-Host ""
Write-Host "Done. Your hosts file and system proxy are back to normal." -ForegroundColor Green
