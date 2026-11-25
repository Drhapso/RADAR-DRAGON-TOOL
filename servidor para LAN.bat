@echo off
setlocal
cd /d "%~dp0"
start "" /min powershell -NoProfile -ExecutionPolicy Bypass -WindowStyle Hidden -File "%~dp0deploy\run-lan.ps1"
timeout /t 2 /nobreak >nul
powershell -NoProfile -ExecutionPolicy Bypass -WindowStyle Hidden -File "%~dp0deploy\validar-index-al-abrir.ps1" -Port 8092
exit /b 0
