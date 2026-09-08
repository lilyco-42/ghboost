# ghboost-tray installer (ASCII only - PowerShell 5.1 reads BOM-less UTF-8 as ANSI)
# Creates a Desktop shortcut and an optional logon autostart entry.
param(
    [string]$InstallDir = "$env:LOCALAPPDATA\ghboost",
    [switch]$NoShortcut,
    [switch]$NoAutostart,
    # Release bundles ship the kernel in .\kernel, so by default NOTHING is
    # downloaded. This matters: our users are exactly the people with bad
    # GitHub access, so making them fetch the kernel from GitHub after
    # installing would defeat the purpose.
    #   -WithKernel  force a fresh download anyway (use it to upgrade)
    #   -NoKernel    skip the kernel entirely ("use existing proxy" mode only)
    [switch]$WithKernel,
    [switch]$NoKernel,
    # -NoLaunch: install everything but do not start the app.
    # Exists so CI can actually EXECUTE this script and assert the resulting
    # layout. Without it the single most failure-prone part of the release
    # (kernel landing under the right name) was never exercised anywhere.
    [switch]$NoLaunch
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

# Kernel layout expected by web.rs::kernel_path() / config_dir():
#   <InstallDir>\bin\mihomo.exe      -> kernel
#   <InstallDir>\mihomo\country.mmdb -> GEOIP rules (GEOIP,TW,DIRECT needs this!)
#   <InstallDir>\mihomo\geosite.dat  -> geosite rules
# Without country.mmdb the GEOIP rules silently never match, which means
# Taiwan-local sites would be sent through the proxy - slower, and some
# banks block it as a foreign login. So the rule DB is not optional.
$BinDir = Join-Path $InstallDir "bin"
$CfgDir = Join-Path $InstallDir "mihomo"
$Bundled = Join-Path $PSScriptRoot "kernel"

if ($NoKernel) {
    Write-Host "Skipping kernel (-NoKernel). Only 'Use existing proxy' will work."
} elseif ((Test-Path $Bundled) -and -not $WithKernel) {
    # Preferred path: take the kernel out of the release bundle, no network.
    New-Item -ItemType Directory -Force -Path $BinDir | Out-Null
    New-Item -ItemType Directory -Force -Path $CfgDir | Out-Null
    Copy-Item (Join-Path $Bundled "bin\*") $BinDir -Force -Recurse
    Copy-Item (Join-Path $Bundled "mihomo\*") $CfgDir -Force -Recurse
    Write-Host "Kernel (bundled) -> $BinDir\mihomo.exe"
} elseif ($WithKernel) {
    [Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12
    $ProgressPreference = "SilentlyContinue"

    New-Item -ItemType Directory -Force -Path $BinDir | Out-Null
    New-Item -ItemType Directory -Force -Path $CfgDir | Out-Null

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
} else {
    Write-Host ""
    Write-Host "NOTE: no kernel in this package and -WithKernel was not given." -ForegroundColor Yellow
    Write-Host "      'Import subscription' will not work; 'Use existing proxy' will." -ForegroundColor Yellow
}

# Normalise the kernel filename. Both the release bundle and the upstream zip can
# ship mihomo-windows-amd64-compatible.exe, but web.rs::kernel_path() looks for
# exactly mihomo.exe - without this the kernel is present yet never found, and
# "Import subscription" fails with a confusing "no kernel" error.
$KernelExe = Join-Path $BinDir "mihomo.exe"
if (-not (Test-Path $KernelExe)) {
    $Cand = Get-ChildItem -Path $BinDir -Filter "mihomo*.exe" -File -ErrorAction SilentlyContinue | Select-Object -First 1
    if ($Cand) {
        Copy-Item $Cand.FullName $KernelExe -Force
        Write-Host "Kernel renamed -> $KernelExe"
    }
}

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

if (-not $NoAutostart) {
    # HKCU Run via PowerShell cmdlet (no reg.exe - it is blacklisted in CI sandboxes
    # and requires no external process anyway).
    $RunKey = "HKCU:\Software\Microsoft\Windows\CurrentVersion\Run"
    Set-ItemProperty -Path $RunKey -Name "ghboost" -Value "`"$Target`" --no-browser"
    Write-Host "Autostart -> HKCU Run (--no-browser: service starts, no popup on logon)"
    Write-Host "NOTE: autostart runs unelevated. hosts write needs admin - use the"
    Write-Host "      panel button when you actually accelerate."
}

# Self-check: turn a silent failure into a visible one.
#
# Why this exists: the first thing the user sees after installing is the panel.
# If the kernel did not land under the exact path the app looks for, the panel
# just says "kernel not found" and the user has no idea what to do - while the
# developer never reproduces it (the dev machine's copy was renamed by hand).
# Now the final layout is asserted right after install and a mismatch exits
# non-zero, which also lets CI assert it.
$Checks = @(
    @{ Name = "tray app";     Path = $Target },
    @{ Name = "kernel";       Path = (Join-Path $BinDir "mihomo.exe") },
    @{ Name = "GEOIP db";     Path = (Join-Path $CfgDir "country.mmdb") },
    @{ Name = "geosite db";   Path = (Join-Path $CfgDir "geosite.dat") }
)
if ($NoKernel) { $Checks = @($Checks[0]) }

$Bad = @()
Write-Host ""
Write-Host "Install self-check:"
foreach ($c in $Checks) {
    if (Test-Path $c.Path) {
        Write-Host ("  [OK]   {0}  {1}" -f $c.Name, $c.Path) -ForegroundColor Green
    } else {
        Write-Host ("  [FAIL] {0}  missing: {1}" -f $c.Name, $c.Path) -ForegroundColor Red
        $Bad += $c.Name
    }
}

if ($Bad.Count -gt 0) {
    Write-Host ""
    Write-Host ("Install incomplete, missing: {0}" -f ($Bad -join ", ")) -ForegroundColor Red
    if (-not $NoKernel) {
        Write-Host "If this is the full release bundle, check that kernel\bin and" -ForegroundColor Yellow
        Write-Host "kernel\mihomo both exist inside the zip." -ForegroundColor Yellow
    }
    exit 1
}

Write-Host ""
Write-Host "Done." -ForegroundColor Green
if (-not $NoLaunch) {
    Write-Host "Launching now..."
    Start-Process $Target
}
exit 0
