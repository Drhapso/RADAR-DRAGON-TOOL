@echo off
chcp 65001 >nul
setlocal
pushd "%~dp0"
title Radar del Dragon - Servicio Local Python (sin .exe)

start "" /min powershell -NoProfile -ExecutionPolicy Bypass -WindowStyle Hidden -File "%~dp0deploy\run-lan.ps1" -LocalOnly -OpenBrowser

timeout /t 2 /nobreak >nul
if exist "%~dp0deploy\validar-index-al-abrir.ps1" (
    powershell -NoProfile -ExecutionPolicy Bypass -WindowStyle Hidden -File "%~dp0deploy\validar-index-al-abrir.ps1" -Port 8092
)
popd
exit /b 0
