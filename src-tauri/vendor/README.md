# vendor/

Binarios de terceros que van en el instalador. No están en git: bajalos acá antes de compilar.

## scrcpy-server.jar

- Versión: **5.0.1** (tiene que coincidir con `scrcpy_version` en
  `src-tauri/src/transport/server.rs`).
- Origen: https://github.com/Genymobile/scrcpy/releases — licencia Apache-2.0.

```bash
curl -fL -o scrcpy-server.jar \
  https://github.com/Genymobile/scrcpy/releases/download/v5.0.1/scrcpy-server-v5.0.1
```

## ffmpeg/ffmpeg.exe y ffmpeg/ffplay.exe

Una build de FFmpeg para Windows x64 (por ejemplo, la "essentials" de https://www.gyan.dev/ffmpeg/builds/).
Copiá `ffmpeg.exe` y `ffplay.exe` de su carpeta `bin/` a `vendor/ffmpeg/`.
