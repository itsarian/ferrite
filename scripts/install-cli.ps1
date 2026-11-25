# Ferrite Windows CLI & Native Host Installer
[CmdletBinding()]
param(
    [string]$InstallDir = ""
)

$ErrorActionPreference = "Stop"

if (-not $InstallDir) {
    if ($env:LOCALAPPDATA) {
        $InstallDir = Join-Path $env:LOCALAPPDATA "Ferrite\bin"
    } else {
        $InstallDir = Join-Path $HOME ".ferrite\bin"
    }
}

Write-Host "==> Installing Ferrite binaries to: $InstallDir" -ForegroundColor Cyan
New-Item -ItemType Directory -Path $InstallDir -Force | Out-Null

$ScriptDir = Split-Path -Parent $MyInvocation.MyCommand.Definition

$binaries = @("ferrite.exe", "ferrite-native-host.exe")
foreach ($bin in $binaries) {
    $src = Join-Path $ScriptDir $bin
    if (Test-Path $src) {
        Copy-Item -Path $src -Destination $InstallDir -Force
        Write-Host "  Installed $bin" -ForegroundColor Green
    }
}

# Add to user PATH if not present
$userPath = [Environment]::GetEnvironmentVariable("Path", [EnvironmentVariableTarget]::User)
$pathEntries = if ($userPath) { $userPath -split ';' } else { @() }
if ($pathEntries -notcontains $InstallDir) {
    Write-Host "==> Adding $InstallDir to User PATH..." -ForegroundColor Cyan
    $newPath = if ($userPath) { "$userPath;$InstallDir" } else { $InstallDir }
    [Environment]::SetEnvironmentVariable("Path", $newPath, [EnvironmentVariableTarget]::User)
    $env:PATH = "$env:PATH;$InstallDir"
    Write-Host "  Added to User PATH. (Restart open terminals for PATH changes to take effect)" -ForegroundColor Yellow
}

Write-Host ""
Write-Host "Ferrite installed successfully!" -ForegroundColor Green
Write-Host "  CLI:         $InstallDir\ferrite.exe"
Write-Host "  Native Host: $InstallDir\ferrite-native-host.exe"
Write-Host ""
Write-Host "To register the browser native messaging host, run:" -ForegroundColor Cyan
Write-Host "  ferrite native-host-install"
