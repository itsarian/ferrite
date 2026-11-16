@echo off
set SCRIPT_DIR=%~dp0
if exist "%SCRIPT_DIR%uninstall.ps1" (
    powershell -NoProfile -ExecutionPolicy Bypass -File "%SCRIPT_DIR%uninstall.ps1" %*
) else (
    powershell -NoProfile -ExecutionPolicy Bypass -File "%SCRIPT_DIR%uninstall-cli.ps1" %*
)
