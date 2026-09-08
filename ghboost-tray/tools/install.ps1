# ghboost-tray installer (ASCII only - PowerShell 5.1 reads BOM-less UTF-8 as ANSI)
# Creates a Desktop shortcut and an optional logon autostart entry.
param(
    [string]$InstallDir = "$env:LOCALAPPDATA\ghboost",
    [switch]$NoShortcut,
    [switch]$NoAutostart,
    # Download the mihomo kernel + geo rule databases (~31MB).
    # Required for the "Import subscription" path. Without it the panel falls
    # back to "Use existing proxy" only.
    [switch]$WithKernel
)

$ErrorActionPreference = "Stop"
$Src = Join-Path $PSScriptRoot "..\target\release\ghboost-tray.exe"

if (-not (Test-Path $Src)) {
    # fall back to the copy delivered with the release bundle
    $Src = Join-Path $PSScriptRoot "ghboost-tray.exe"
}
if (-not (Test-Path $Src)) {
    Write-Host "ERROR: ghboost-tray.exe not found. Build it first: cargo build --release" -ForegroundColor Red
    exit 1
}

Write-Host "Source : $Src"

New-Item -ItemType Directory -Force -Path $InstallDir | Out-Null
$Target = Join-Path $InstallDir "ghboost-tray.exe"
Copy-Item $Src $Target -Force
Write-Host "Installed -> $Target"

if (-not $NoShortcut) {
    $Desktop = [Environment]::GetFolderPath("Desktop")
    $Lnk = Join-Path $Desktop "ghboost.lnk"
    $Wsh = New-Object -ComObject WScript.Shell
    $Sc = $Wsh.CreateShortcut($Lnk)
    $Sc.TargetPath = $Target
    $Sc.WorkingDirectory = $InstallDir
    $Sc.Description = "ghboost - one-click access acceleration"
    $Sc.Save()
    Write-Host "Desktop shortcut -> $Lnk"
}

if ($WithKernel) {
    # Kernel layout expected by web.rs::kernel_path() / config_dir():
    #   <InstallDir>\bin\mihomo.exe      -> kernel
    #   <InstallDir>\mihomo\country.mmdb -> GEOIP rules (GEOIP,TW,DIRECT needs this!)
    #   <InstallDir>\mihomo\geosite.dat  -> geosite rules
    # Without country.mmdb the GEOIP rules silently never match, which means
    # Taiwan-local sites would be sent through the proxy - slower, and some
    # banks block it as a foreign login. So the rule DB is not optional.
    $BinDir = Join-Path $InstallDir "bin"
    $CfgDir = Join-Path $InstallDir "mihomo"
    New-Item -ItemType Directory -Force -Path $BinDir | Out-Null
    New-Item -ItemType Directory -Force -Path $CfgDir | Out-Null

    [Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12
    $ProgressPreference = "SilentlyContinue"

    $Zip = Join-Path $env:TEMP "mihomo.zip"
    $KernelUrl = "https://github.com/MetaCubeX/mihomo/releases/download/v1.19.30/mihomo-windows-amd64-compatible-v1.19.30.zip"
    Write-Host "Downloading mihomo kernel (~18.5MB)..."
    try {
        Invoke-WebRequest -Uri $KernelUrl -OutFile $Zip -UseBasicParsing
    } catch {
        Write-Host "ERROR: kernel download failed: $($_.Exception.Message)" -ForegroundColor Red
        Write-Host "       You can still use the panel's 'Use existing proxy' mode." -ForegroundColor Yellow
        $Zip = $null
    }

    if ($Zip) {
        Expand-Archive -Path $Zip -DestinationPath $BinDir -Force
        Remove-Item $Zip -Force
        Write-Host "Kernel -> $BinDir\mihomo.exe"
    }

    foreach ($f in @("country.mmdb", "geosite.dat")) {
        $Dst = Join-Path $CfgDir $f
        $Url = "https://github.com/MetaCubeX/meta-rules-dat/releases/download/latest/$f"
        Write-Host "Downloading $f..."
        try {
            Invoke-WebRequest -Uri $Url -OutFile $Dst -UseBasicParsing
            Write-Host "  -> $Dst"
        } catch {
            Write-Host "  WARN: $f failed: $($_.Exception.Message)" -ForegroundColor Yellow
        }
    }
    Write-Host ""
    Write-Host "NOTE: mihomo is GPL-3.0. Redistributing it requires complying with" -ForegroundColor DarkYellow
    Write-Host "      that license (provide source / written offer)." -ForegroundColor DarkYellow
}

if (-not $NoAutostart) {
    # HKCU Run via PowerShell cmdlet (no reg.exe - it is blacklisted in CI sandboxes
    # and requires no external process anyway).
    $RunKey = "HKCU:\Software\Microsoft\Windows\CurrentVersion\Run"
    Set-ItemProperty -Path $RunKey -Name "ghboost" -Value "`"$Target`" --no-browser"
    Write-Host "Autostart -> HKCU Run (--no-browser: service starts, no popup on logon)"
    Write-Host "NOTE: autostart runs unelevated. hosts write needs admin - use the"
    Write-Host "      panel button when you actually accelerate."
}

Write-Host ""
Write-Host "Done. Launching now..." -ForegroundColor Green
Start-Process $Target
