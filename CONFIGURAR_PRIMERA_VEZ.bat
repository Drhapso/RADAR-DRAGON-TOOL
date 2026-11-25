@echo off
setlocal
cd /d "%~dp0"
net session >nul 2>&1
if %errorlevel% neq 0 (
    echo Solicitando permisos de Administrador para verificar/instalar dependencias faltantes...
    powershell -NoProfile -Command "Start-Process -FilePath '%~f0' -Verb RunAs"
    exit /b
)
powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0configurar_primera_vez.ps1" %*
set "CODE=%ERRORLEVEL%"
echo.
if not "%CODE%"=="0" echo [ERROR] Revise el mensaje anterior antes de cerrar.
pause
exit /b %CODE%
