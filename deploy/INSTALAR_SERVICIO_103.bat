@echo off
setlocal
cd /d "%~dp0"
net session >nul 2>&1
if %errorlevel% neq 0 (
    echo Solicitando permisos de Administrador para instalar Dragon-Radar-LAN en 172.23.174.103:8092...
    powershell -NoProfile -Command "Start-Process -FilePath '%~f0' -Verb RunAs"
    exit /b
)
powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0INSTALAR_SERVICIO_103.ps1" %*
echo.
pause
