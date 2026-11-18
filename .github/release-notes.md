## Aguará 0.1.10

**Ahora también para Mac.** Instaladores para Windows 10/11 (64 bits) y macOS 11 o posterior (Intel y Apple Silicon, un solo archivo).

**Instalar en Windows:** bajá `Aguara_0.1.10_x64-setup.exe` y abrilo. No pide permisos de administrador. Si aparece "Windows protegió su PC", tocá "Más información" → "Ejecutar de todas formas" (el instalador no está firmado).

**Instalar en Mac:** bajá `Aguara_0.1.10_macOS.dmg`, abrilo y arrastrá Aguará a Aplicaciones. La primera vez abrila con clic derecho → "Abrir" (la app no está firmada por Apple). Si macOS no te deja, andá a Ajustes del Sistema → Privacidad y seguridad → "Abrir igualmente". Necesita internet la primera vez para descargar adb y FFmpeg.

La guía completa está en `LEEME-Aguara.txt`.

### Novedades
- **Versión para Mac**, con los atajos de siempre usando ⌘ (⌘V para pegar, ⌘ + rueda para el tamaño). Las capturas van a Imágenes/Aguará.
- **Diseño a elección:** botón "Diseño" para elegir columnas, tamaño y espacio, y qué botones se ven en la barra de acciones y en qué orden.
- **Capturas con aviso:** al sacar una captura aparece un aviso con "Abrir carpeta".
- Arreglos al aceptar solicitudes de Facebook.

### Arreglos
- La ventana ya no se congela mientras se busca teléfonos, se saca una captura, se instala un APK o se envía un archivo.
- Un teléfono podía quedar "conectando" para siempre si el video no arrancaba la primera vez.
- "Detener" ahora detiene el video: antes los teléfonos se volvían a conectar solos a los 3 segundos.
- Con AltGr se pueden escribir @, # y { en el teléfono.
- "Volver al diseño original" restaura también los botones destildados.
- Esc al cerrar el panel de Diseño ya no manda "Atrás" al teléfono.
- Las rutas de APK o archivos pegadas entre comillas ahora funcionan.

### Software de terceros incluido
- **FFmpeg** (`ffmpeg.exe`, `ffplay.exe`, solo en el instalador de Windows), licencia GPL. Código fuente: https://ffmpeg.org/download.html — build de https://www.gyan.dev/ffmpeg/builds/. En Mac, FFmpeg se descarga la primera vez.
- **scrcpy-server 5.0.1**, licencia Apache-2.0. Código fuente: https://github.com/Genymobile/scrcpy
