# Aguará

App de escritorio para ver y manejar desde la PC varios teléfonos Android conectados por USB.
Hecha con Tauri v2 + Rust y una UI en TypeScript. Usa el
[scrcpy-server](https://github.com/Genymobile/scrcpy) oficial (Apache-2.0): sin root y sin
instalar nada en el teléfono.

## Qué hace

- Grilla con el video en vivo de todos los teléfonos; tocar, deslizar, escribir y pegar desde
  la PC.
- Control en grupo: lo que hagas en uno se repite en todos los seleccionados.
- Nombres y números por teléfono, orden con arrastrar, vista ampliada con doble clic.
- Abrir links (posts, perfiles, videos) en la app que corresponde.
- Cambiar de cuenta en Instagram, Facebook, X y otras con un clic.
- Aceptar solicitudes de amistad de Facebook, con botón para detener.
- Audio del teléfono en la PC, capturas por fecha, instalar APK, enviar archivos.
- Scripts (`tap`, `swipe`, `text`, `key`, `sleep`, `loop`, toque por imagen) con grabación.
- Desbloquear pantalla y botón de pánico (borra los datos de las apps de todos los teléfonos).
- Español e inglés.

La guía de uso para quien instala la app está en [`LEEME-Aguara.txt`](LEEME-Aguara.txt).

## Requisitos para compilar

1. **Rust** stable — https://rustup.rs
2. **Node.js ≥ 18** y npm.
3. Windows 10/11 con WebView2 (viene con Windows 11).
4. Los binarios de `src-tauri/vendor/` (no están en git; ver
   [`src-tauri/vendor/README.md`](src-tauri/vendor/README.md)).

`adb` no hace falta: si no hay uno funcionando, la app descarga las platform-tools oficiales
de Google la primera vez.

## Desarrollo

```bash
npm install
npm run tauri dev
```

Solo la UI, con teléfonos simulados (sin Tauri): `npm run dev` y abrir
`http://localhost:5173`.

## Instalador

```bash
npm run tauri build -- --features custom-protocol
```

Genera `src-tauri/target/release/bundle/nsis/Aguará_<versión>_x64-setup.exe`.

## Tests

```bash
cd src-tauri && cargo test
npx tsc --noEmit
```

## Estructura

```
src/                UI (TypeScript, Vite)
src-tauri/
  src/
    domain/         mensajes de control de scrcpy y lectura de la UI de Android
    transport/      adb, arranque de scrcpy-server, conexiones
    media/          decodificación de video (ffmpeg)
    app/            sesiones por teléfono
    commands.rs     comandos que llama la UI
  vendor/           scrcpy-server y ffmpeg (no versionados)
  icons/            ícono del zorro
```
