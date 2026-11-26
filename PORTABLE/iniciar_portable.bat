@echo off
setlocal EnableDelayedExpansion
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

if /I not "%PROCESSOR_ARCHITECTURE%"=="AMD64" (
    echo [ERROR] Esta version requiere Windows de 64 bits para procesadores x64.
    echo         Consulta al administrador si usas una arquitectura diferente.
    pause
    popd
    exit /b 1
)

if exist "%~dp0dragon.exe" (
    "%~dp0dragon.exe" ui
    set "RESULT=%ERRORLEVEL%"
    if not "!RESULT!"=="0" (
        echo.
        echo [ERROR] Radar se cerro con un error. Anota el mensaje anterior y contacta al administrador.
        pause
    )
    popd
    exit /b !RESULT!
) else (
    echo [ERROR] No se encontro dragon.exe dentro de la carpeta PORTABLE.
    echo         Ejecuta compilador.ps1 en la raiz del proyecto para generarlo.
    pause
    popd
    exit /b 1
)
