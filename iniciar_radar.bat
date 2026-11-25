@echo off
chcp 65001 >nul
cd /d "%~dp0"
title Dragon Radar Tool - Tactical Search HUD
color 0A
cls
echo ======================================================================
echo                DRAGON RADAR TOOL - TACTICAL HUD
echo   Buscador Hibrido de Alta Velocidad con MFT y Semantica IA
echo ======================================================================
echo.
echo Iniciando servidor tactico del Radar del Dragon...
echo Abriendo pantalla del radar en tu navegador predeterminado...
echo.
echo  * HUD Web       : http://127.0.0.1:8088
echo  * Named Pipe IPC: \\.\pipe\dragon_radar_mft
echo.
echo [NOTA]: Manten esta ventana abierta mientras utilices el radar.
echo        Para cerrar el radar, simplemente cierra esta ventana.
echo ======================================================================
echo.

if exist "dragon.exe" (
    dragon.exe ui
) else if exist "target\release\dragon.exe" (
    target\release\dragon.exe ui
) else (
    echo [ERROR] No se encontro dragon.exe. Por favor compila con 'cargo build --release'.
    pause
)
