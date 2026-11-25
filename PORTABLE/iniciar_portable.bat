@echo off
chcp 65001 >nul
pushd "%~dp0"
title Radar del Dragon - Edicion PORTABLE
color 0A
cls
echo ======================================================================
echo           RADAR DEL DRAGON - EDICION PORTABLE (STANDALONE)
echo ======================================================================
echo.
echo Iniciando el radar localmente en %COMPUTERNAME% (127.0.0.1)...
echo Esta edicion funciona de forma autonoma en cualquier equipo Windows
echo sin funciones de red LAN.
echo.
echo [NOTA]: Manten esta ventana abierta mientras utilizas el radar.
echo         Para cerrarlo, simplemente cierra esta ventana.
echo ======================================================================
echo.

if exist "%~dp0dragon.exe" (
    "%~dp0dragon.exe" ui
) else (
    echo [ERROR] No se encontro dragon.exe dentro de la carpeta PORTABLE.
    echo         Ejecuta compilador.ps1 en la raiz del proyecto para generarlo.
    pause
)
popd
