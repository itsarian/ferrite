# Ferrite Windows CLI & Native Host Uninstaller
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

Write-Host "==> Uninstalling Ferrite from: $InstallDir" -ForegroundColor Yellow

$binaries = @("ferrite.exe", "ferrite-native-host.exe")
foreach ($bin in $binaries) {
    $target = Join-Path $InstallDir $bin
    if (Test-Path $target) {
        Remove-Item -Path $target -Force
        Write-Host "  Removed $bin" -ForegroundColor Green
    }
}

# Remove directory if empty
if ((Test-Path $InstallDir) -and (Get-ChildItem -Path $InstallDir | Measure-Object).Count -eq 0) {
    Remove-Item -Path $InstallDir -Force -Recurse
    $parent = Split-Path -Parent $InstallDir
    if ((Test-Path $parent) -and (Get-ChildItem -Path $parent | Measure-Object).Count -eq 0) {
        Remove-Item -Path $parent -Force -Recurse
    }
}

# Remove from User PATH
$userPath = [Environment]::GetEnvironmentVariable("Path", [EnvironmentVariableTarget]::User)
if ($userPath) {
    $pathEntries = $userPath -split ';' | Where-Object { $_ -ne $InstallDir -and $_ -ne "" }
    $newPath = $pathEntries -join ';'
    [Environment]::SetEnvironmentVariable("Path", $newPath, [EnvironmentVariableTarget]::User)
}

Write-Host "Ferrite uninstalled successfully." -ForegroundColor Green
