//! Decodificación de video: los paquetes H.264 de scrcpy entran por la stdin de `ffmpeg`, que
//! los re-codifica a MJPEG por stdout. Cada frame sale como un JPEG chico (decenas de KB),
//! mucho más barato de mandar a la UI que RGBA crudo.
//!
//! Latencia: el parser H.264 de ffmpeg da un frame por terminado recién al ver el inicio del
//! siguiente, así que con la pantalla quieta el último cambio quedaría retenido. Por eso se
//! pide a scrcpy la cabecera por paquete y, tras cada frame, se agrega un AUD (delimitador de
//! access unit): ffmpeg lo entrega en el acto.

use std::io::{Read, Write};
use std::process::{Command, Stdio};

/// Un frame codificado en JPEG, con su tamaño (= tamaño del video del dispositivo).
pub struct Frame {
    pub width: u32,
    pub height: u32,
    pub jpeg: Vec<u8>,
}

/// Resultado de un match de plantilla: centro en coords del frame + score [0,1].
pub struct Match {
    pub x: i32,
    pub y: i32,
    pub score: f32,
}

/// Busca `template_path` dentro de un frame RGBA por correlación cruzada normalizada en
/// escala de grises. Trabaja a resolución reducida por velocidad y mapea el resultado a
/// coordenadas del frame completo. Devuelve el mejor match si supera `threshold`.
pub fn match_template(
    frame_rgba: &[u8],
    fw: u32,
    fh: u32,
    template_path: &str,
    threshold: f32,
) -> std::io::Result<Option<Match>> {
    use image::imageops::{grayscale, resize, FilterType};
    use image::{GenericImageView, ImageBuffer, Rgba};

    let frame: ImageBuffer<Rgba<u8>, Vec<u8>> =
        ImageBuffer::from_raw(fw, fh, frame_rgba.to_vec()).ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::InvalidData, "frame rgba inválido")
        })?;
    let tpl = image::open(template_path)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e.to_string()))?;

    // Escala de trabajo: reducir el frame a ~480 px de ancho.
    let scale = if fw > 480 { 480.0 / fw as f32 } else { 1.0 };
    let gw = (fw as f32 * scale).round().max(1.0) as u32;
    let gh = (fh as f32 * scale).round().max(1.0) as u32;
    let fg = grayscale(&resize(&frame, gw, gh, FilterType::Triangle));
    let (tw0, th0) = tpl.dimensions();
    let tw = (tw0 as f32 * scale).round().max(1.0) as u32;
    let th = (th0 as f32 * scale).round().max(1.0) as u32;
    if tw > gw || th > gh || tw == 0 || th == 0 {
        return Ok(None);
    }
    let tg = grayscale(&resize(&tpl.to_rgba8(), tw, th, FilterType::Triangle));

    // Media de la plantilla.
    let tpl_px: Vec<f32> = tg.pixels().map(|p| p[0] as f32).collect();
    let tmean = tpl_px.iter().sum::<f32>() / tpl_px.len() as f32;
    let tdev: Vec<f32> = tpl_px.iter().map(|v| v - tmean).collect();
    let tnorm = tdev.iter().map(|v| v * v).sum::<f32>().sqrt();
    if tnorm == 0.0 {
        return Ok(None);
    }

    let fg_px = |x: u32, y: u32| fg.get_pixel(x, y)[0] as f32;
    let mut best = Match { x: 0, y: 0, score: -1.0 };
    for oy in 0..=(gh - th) {
        for ox in 0..=(gw - tw) {
            // Media de la ventana.
            let mut sum = 0.0;
            for j in 0..th {
                for i in 0..tw {
                    sum += fg_px(ox + i, oy + j);
                }
            }
            let wmean = sum / (tw * th) as f32;
            let mut dot = 0.0;
            let mut wnorm2 = 0.0;
            let mut k = 0usize;
            for j in 0..th {
                for i in 0..tw {
                    let wv = fg_px(ox + i, oy + j) - wmean;
                    dot += wv * tdev[k];
                    wnorm2 += wv * wv;
                    k += 1;
                }
            }
            let denom = tnorm * wnorm2.sqrt();
            let score = if denom > 0.0 { dot / denom } else { 0.0 };
            if score > best.score {
                // Centro en coords del frame completo.
                best = Match {
                    x: (((ox + tw / 2) as f32) / scale).round() as i32,
                    y: (((oy + th / 2) as f32) / scale).round() as i32,
                    score,
                };
            }
        }
    }

    Ok(if best.score >= threshold { Some(best) } else { None })
}

/// Flag de scrcpy en `pts_and_flags`: paquete de configuración (SPS/PPS), no es un frame.
const PACKET_FLAG_CONFIG: u64 = 1 << 63;

/// NAL "access unit delimiter" (tipo 9): marca el fin del frame anterior.
const H264_AUD: [u8; 6] = [0, 0, 0, 1, 0x09, 0xF0];

/// Copia los paquetes de scrcpy (`[pts+flags: u64][tamaño: u32][payload]`) a `out` como
/// H.264 Annex-B, agregando un AUD después de cada frame. Termina en EOF o error.
fn pump_scrcpy_packets<R: Read, W: Write>(source: R, out: W) -> std::io::Result<()> {
    pump_packets(source, out, &H264_AUD)
}

/// Copia el payload de cada paquete de scrcpy a `out` (sin cabeceras), con `suffix` tras
/// cada paquete que no sea de configuración.
fn pump_packets<R: Read, W: Write>(mut source: R, mut out: W, suffix: &[u8]) -> std::io::Result<()> {
    let mut header = [0u8; 12];
    let mut payload = Vec::with_capacity(256 * 1024);
    loop {
        source.read_exact(&mut header)?;
        let pts_flags = u64::from_be_bytes(header[..8].try_into().unwrap());
        let len = u32::from_be_bytes(header[8..].try_into().unwrap()) as usize;
        payload.resize(len, 0);
        source.read_exact(&mut payload)?;
        out.write_all(&payload)?;
        if pts_flags & PACKET_FLAG_CONFIG == 0 && !suffix.is_empty() {
            out.write_all(suffix)?;
        }
        out.flush()?;
    }
}

/// Ejecutable de ffmpeg: el que viene con la app, el que la app descargó o el del sistema.
pub fn ffmpeg_exe() -> std::path::PathBuf {
    crate::platform::tool_or_path("ffmpeg")
}

/// Si no hay ffmpeg usable, lo descarga a la carpeta de herramientas de la app.
pub fn ensure_ffmpeg() -> Result<(), String> {
    if crate::platform::tool_available("ffmpeg") {
        return Ok(());
    }
    let dest = crate::platform::tools_dir().ok_or("no se encontró la carpeta de la app")?;
    std::fs::create_dir_all(&dest).map_err(|e| format!("no se pudo crear {}: {e}", dest.display()))?;
    let url = ffmpeg_sidecar::download::ffmpeg_download_url().map_err(|e| e.to_string())?;
    let archive = ffmpeg_sidecar::download::download_ffmpeg_package(url, &dest).map_err(|e| format!("{e:#}"))?;
    ffmpeg_sidecar::download::unpack_ffmpeg(&archive, &dest).map_err(|e| format!("{e:#}"))?;
    for tool in ["ffmpeg", "ffplay"] {
        crate::platform::make_executable(&dest.join(crate::platform::exe_name(tool)));
    }
    Ok(())
}

/// Entrada de audio crudo de scrcpy para ffmpeg/ffplay: PCM s16le, 48 kHz, estéreo.
const RAW_AUDIO_INPUT: [&str; 14] = [
    "-fflags", "nobuffer", "-flags", "low_delay",
    "-probesize", "32", "-analyzeduration", "0",
    "-f", "s16le", "-ar", "48000", "-ch_layout", "stereo",
];

/// Reproduce en la computadora el audio crudo de scrcpy, sin ventana. Devuelve el proceso:
/// matarlo corta el audio. El socket se bombea en su hilo.
///
/// Con ffplay si está (viene en el instalador de Windows). En macOS las descargas de FFmpeg no
/// traen ffplay: ahí ffmpeg mismo manda el audio a la salida del sistema (AudioToolbox).
pub fn play_raw_audio<R: Read + Send + 'static>(source: R) -> std::io::Result<std::process::Child> {
    let mut cmd = match crate::platform::find_tool("ffplay") {
        Some(ffplay) => {
            let mut cmd = Command::new(ffplay);
            cmd.args(["-hide_banner", "-loglevel", "error", "-nodisp", "-autoexit"])
                .args(RAW_AUDIO_INPUT)
                .args(["-i", "pipe:0"]);
            cmd
        }
        None if cfg!(target_os = "macos") => {
            let mut cmd = Command::new(ffmpeg_exe());
            cmd.args(["-hide_banner", "-loglevel", "error"])
                .args(RAW_AUDIO_INPUT)
                .args(["-i", "pipe:0", "-f", "audiotoolbox", "-"]);
            cmd
        }
        None => {
            // Sin ruta conocida: probar el ffplay del PATH.
            let mut cmd = Command::new(crate::platform::exe_name("ffplay"));
            cmd.args(["-hide_banner", "-loglevel", "error", "-nodisp", "-autoexit"])
                .args(RAW_AUDIO_INPUT)
                .args(["-i", "pipe:0"]);
            cmd
        }
    };
    cmd.stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::null());
    crate::transport::hide_window(&mut cmd);
    let mut child = cmd.spawn().map_err(|e| {
        std::io::Error::new(e.kind(), format!("no se pudo abrir el reproductor de audio (ffplay/ffmpeg): {e}"))
    })?;
    let stdin = child.stdin.take().expect("stdin del reproductor");
    std::thread::spawn(move || {
        let _ = pump_packets(source, stdin, &[]);
    });
    Ok(child)
}

/// ffmpeg ya arrancado, convirtiendo el H.264 de un teléfono a JPEG.
pub struct Decoder {
    child: std::process::Child,
}

impl Decoder {
    /// Arranca ffmpeg y empieza a alimentarlo con los paquetes H.264 de scrcpy que llegan por
    /// `source` (en un hilo aparte). Falla enseguida si ffmpeg no está o no se puede ejecutar.
    ///
    /// `quality` es la escala `-q:v` de mjpeg (2 = mejor, 31 = peor).
    pub fn spawn<R: Read + Send + 'static>(source: R, quality: u8) -> std::io::Result<Self> {
        let q = quality.clamp(2, 31).to_string();
        let mut cmd = Command::new(ffmpeg_exe());
        cmd.args([
            "-hide_banner", "-loglevel", "error",
            // Entrada: sin análisis largo del stream y decode en un hilo (el decode multihilo
            // retiene tantos frames como hilos antes de entregar el primero).
            "-threads", "1", "-flags", "low_delay",
            "-probesize", "32", "-analyzeduration", "0",
            "-f", "h264", "-i", "pipe:0",
            // Salida: un JPEG por frame recibido, sin rellenar/duplicar por framerate.
            "-fps_mode", "passthrough",
            // MJPEG exige rango completo; muchos teléfonos marcan el video como rango limitado.
            "-pix_fmt", "yuvj420p",
            "-threads", "1",
            "-f", "image2pipe", "-c:v", "mjpeg", "-q:v", &q,
            "-flush_packets", "1", "pipe:1",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
        crate::transport::hide_window(&mut cmd);
        let mut child = cmd.spawn()?;

        // Alimentar ffmpeg desde el socket, en un hilo aparte.
        let stdin = child.stdin.take().expect("ffmpeg stdin");
        std::thread::spawn(move || {
            let _ = pump_scrcpy_packets(source, stdin);
        });
        Ok(Self { child })
    }

    /// Entrega cada frame como JPEG a `on_frame` hasta que el stream termina. Bloquea; correr
    /// en su hilo.
    pub fn run<F: FnMut(Frame)>(mut self, mut on_frame: F) {
        let Some(mut stdout) = self.child.stdout.take() else { return };
        let mut buf: Vec<u8> = Vec::with_capacity(512 * 1024);
        let mut chunk = vec![0u8; 64 * 1024];
        loop {
            let n = match stdout.read(&mut chunk) {
                Ok(0) | Err(_) => break,
                Ok(n) => n,
            };
            buf.extend_from_slice(&chunk[..n]);
            // Extraer todos los JPEG completos del buffer.
            loop {
                // Re-sincronizar al próximo SOI (FF D8) si hay basura delante.
                match buf.windows(2).position(|w| w == [0xFF, 0xD8]) {
                    Some(0) => {}
                    Some(p) => {
                        buf.drain(..p);
                    }
                    None => {
                        let keep = usize::from(buf.last() == Some(&0xFF));
                        buf.drain(..buf.len() - keep);
                        break;
                    }
                }
                match scan_jpeg(&buf) {
                    Some((len, w, h)) => {
                        let jpeg: Vec<u8> = buf.drain(..len).collect();
                        on_frame(Frame { width: w, height: h, jpeg });
                    }
                    None => break, // incompleto: esperar más datos
                }
            }
        }
    }
}

impl Drop for Decoder {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Si `buf` empieza con un JPEG completo, devuelve (longitud, ancho, alto).
/// Recorre los segmentos de cabecera y, tras SOS, los datos entrópicos hasta EOI.
fn scan_jpeg(buf: &[u8]) -> Option<(usize, u32, u32)> {
    let (mut w, mut h) = (0u32, 0u32);
    let mut i = 2; // tras SOI
    loop {
        if i + 1 >= buf.len() {
            return None;
        }
        if buf[i] != 0xFF {
            return None;
        }
        let m = buf[i + 1];
        if m == 0xFF {
            i += 1; // relleno
            continue;
        }
        if m == 0xD9 {
            return (w > 0 && h > 0).then_some((i + 2, w, h));
        }
        if i + 3 >= buf.len() {
            return None;
        }
        let seg = u16::from_be_bytes([buf[i + 2], buf[i + 3]]) as usize;
        // SOF0..SOF3: alto y ancho.
        if (0xC0..=0xC3).contains(&m) && i + 8 < buf.len() {
            h = u16::from_be_bytes([buf[i + 5], buf[i + 6]]) as u32;
            w = u16::from_be_bytes([buf[i + 7], buf[i + 8]]) as u32;
        }
        i += 2 + seg;
        if m == 0xDA {
            // Datos entrópicos: FF 00 y RSTn son parte del dato; cualquier otro FF xx es marcador.
            loop {
                if i + 1 >= buf.len() {
                    return None;
                }
                if buf[i] == 0xFF {
                    let n = buf[i + 1];
                    if n == 0x00 || (0xD0..=0xD7).contains(&n) {
                        i += 2;
                        continue;
                    }
                    break;
                }
                i += 1;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{pump_scrcpy_packets, scan_jpeg, H264_AUD, PACKET_FLAG_CONFIG};

    #[test]
    fn pump_strips_headers_and_delimits_frames() {
        let mut input = Vec::new();
        // Paquete de configuración (sin AUD después) y un frame (con AUD).
        input.extend(PACKET_FLAG_CONFIG.to_be_bytes());
        input.extend(3u32.to_be_bytes());
        input.extend([0xA, 0xB, 0xC]);
        input.extend(42u64.to_be_bytes());
        input.extend(2u32.to_be_bytes());
        input.extend([0xD, 0xE]);
        let mut out = Vec::new();
        let r = pump_scrcpy_packets(&input[..], &mut out);
        assert!(r.is_err()); // termina en EOF
        let mut expected = vec![0xA, 0xB, 0xC, 0xD, 0xE];
        expected.extend(H264_AUD);
        assert_eq!(out, expected);
    }

    #[test]
    fn scans_minimal_jpeg() {
        // SOI, SOF0 (alto 2, ancho 3), SOS, datos con FF00, EOI.
        let mut j = vec![0xFF, 0xD8];
        j.extend([0xFF, 0xC0, 0x00, 0x0B, 0x08, 0x00, 0x02, 0x00, 0x03, 0x01, 0x01, 0x11, 0x00]);
        j.extend([0xFF, 0xDA, 0x00, 0x08, 0x01, 0x01, 0x00, 0x00, 0x3F, 0x00]);
        j.extend([0x12, 0xFF, 0x00, 0x34]);
        j.extend([0xFF, 0xD9]);
        let len = j.len();
        j.extend([0xFF, 0xD8, 0x00]); // inicio del siguiente frame
        assert_eq!(scan_jpeg(&j), Some((len, 3, 2)));
        assert_eq!(scan_jpeg(&j[..len - 1]), None);
    }
}
