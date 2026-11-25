@echo off
chcp 65001 >nul
pushd "%~dp0"
title Radar del Dragon - Buscador Local del Equipo
color 0A
cls
echo ======================================================================
echo                RADAR DEL DRAGON - BUSCADOR DE ARCHIVOS
echo ======================================================================
echo.
echo Iniciando el radar en este equipo (%COMPUTERNAME%)...
echo Los archivos se indexan y buscan localmente en esta computadora.
echo.
echo [NOTA]: Manten esta ventana abierta mientras utilices el radar.
echo         Para cerrarlo, simplemente cierra esta ventana.
echo ======================================================================
echo.

if exist "target\release\dragon.exe" (
    copy /Y "target\release\dragon.exe" "dragon.exe" >nul 2>&1
    "target\release\dragon.exe" ui
) else if exist "dragon.exe" (
    "dragon.exe" ui
) else (
    echo [ERROR] No se encontro dragon.exe en esta carpeta.
    pause
)
popd
