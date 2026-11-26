# Guía de instalación vigente

La guía anterior describía compilación y despliegue de `dragon.exe`, modo de archivos compartidos y valores de red como si estuvieran confirmados. Ese procedimiento no es la vía para el cliente LAN Python offline actual.

## Cliente Windows x64 offline

Usa `dist/Radar-del-Dragon-Cliente-Offline-Python314-win-x64.zip`.

1. Compara su SHA-256 con `dist/Radar-del-Dragon-Cliente-Offline-Python314-win-x64.zip.sha256.txt` mediante un canal confiable.
2. Extrae el ZIP en una ruta local con permisos de escritura del usuario.
3. Ejecuta `INSTALL_OFFLINE.bat`.
4. Si finaliza correctamente, abre `http://127.0.0.1:8092`.

El paquete incorpora Python 3.14.10 x64, el código Python, la interfaz y las dependencias offline. Instala para el usuario actual, no activa políticas de ejecución ni pide elevación de forma intencionada. Si CrowdStrike o una política corporativa bloquea un componente, detente y solicita a TI una distribución aprobada.

## Operación LAN

La activación del cliente utiliza su agente local. Los servidores de entrada solo deben servir UI y recursos aprobados; no deben recibir consultas, rutas ni previews de los usuarios. El principal, respaldo, proxy, TLS, DNS, puertos y firewall requieren confirmación presencial o remota dentro de la LAN por TI. Los valores en archivos de ejemplo no certifican la configuración vigente.

No compilar ni desplegar `dragon.exe` como alternativa de cliente mientras CrowdStrike lo bloquee. No cambiar a `shared_server`, no crear servicios `SYSTEM`, ni modificar Firewall/proxy/certificados para hacer pasar las pruebas sin autorización.

## Documentos de referencia

- `README.md`: resumen funcional y uso.
- `DOCUMENTACION.md`: arquitectura, paquete, privacidad y límites.
- `INSTRUCCIONES_VALIDACION_CODEX.txt`: comprobaciones desde Codex y formato del informe.
- `INSTRUCCIONES_ANTIGRAVITY_LAN.txt`: secuencia de pruebas en la LAN, criterios de parada y entrega.
