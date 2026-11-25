@echo off
chcp 65001 >nul
cd /d "%~dp0"
title Dragon Radar Tool - Indexador de Unidades Locales
color 0A
cls
echo ======================================================================
echo                DRAGON RADAR TOOL - INDEXADOR DE EQUIPO
echo        Indexacion de Unidades Locales (C:, D:, E:...) sin SO
echo ======================================================================
echo.
echo Iniciando proceso de escaneo e indexacion de todas las unidades locales...
echo Se omitiran automaticamente los archivos del Sistema Operativo Windows.
echo.
echo Presiona cualquier tecla para comenzar o cierra la ventana para cancelar.
pause >nul

powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0indexar_equipo.ps1"

echo.
echo ======================================================================
echo Indexacion finalizada. Presiona cualquier tecla para salir.
echo ======================================================================
pause >nul
