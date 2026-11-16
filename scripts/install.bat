@echo off
set SCRIPT_DIR=%~dp0
if exist "%SCRIPT_DIR%install.ps1" (
    powershell -NoProfile -ExecutionPolicy Bypass -File "%SCRIPT_DIR%install.ps1" %*
) else (
    powershell -NoProfile -ExecutionPolicy Bypass -File "%SCRIPT_DIR%install-cli.ps1" %*
)
