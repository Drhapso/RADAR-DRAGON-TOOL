@echo off
chcp 65001 >nul
pushd "%~dp0"
title Radar del Dragon - Buscador Local del Equipo

if exist "target\release\dragon.exe" (
    copy /Y "target\release\dragon.exe" "dragon.exe" >nul 2>&1
    start "" /min "dragon.exe" ui
) else if exist "dragon.exe" (
    start "" /min "dragon.exe" ui
) else (
    echo [ERROR] No se encontro dragon.exe en esta carpeta.
    pause
    popd
    exit /b 1
)

timeout /t 2 /nobreak >nul
if exist "deploy\validar-index-al-abrir.ps1" (
    powershell -NoProfile -ExecutionPolicy Bypass -WindowStyle Hidden -File "%~dp0deploy\validar-index-al-abrir.ps1" -Port 8092
)
popd
exit /b 0
