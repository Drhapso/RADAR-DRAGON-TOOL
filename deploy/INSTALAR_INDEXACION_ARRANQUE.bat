@echo off
setlocal
cd /d "%~dp0"
net session >nul 2>&1
if %errorlevel% neq 0 (
    echo Solicitando permisos de Administrador para instalar el arranque automatico de indexacion...
    powershell -NoProfile -Command "Start-Process -FilePath '%~f0' -Verb RunAs"
    exit /b
)
powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0instalar-servicio-indexacion-arranque.ps1" %*
echo.
pause
