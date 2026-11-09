//! Ciclo de vida del scrcpy-server en el dispositivo (camino limpio, scrcpy upstream).
//!
//! Secuencia P0:
//!   1. push del jar a /data/local/tmp/
//!   2. adb reverse localabstract:<scid> tcp:<puerto> y escuchar en el PC
//!   3. app_process arranca el server con las opciones (video on, control on)
//!   4. el server conecta el/los socket(s) de vuelta al PC (tunnel_forward=false)

use std::path::PathBuf;
use std::process::{Child, Command, Stdio};

use super::{AdbClient, Result, TransportError};

const REMOTE_JAR: &str = "/data/local/tmp/scrcpy-server.jar";
const SERVER_CLASS: &str = "com.genymobile.scrcpy.Server";

/// Opciones del server (subconjunto P0; todas configurables más adelante, §3.7).
#[derive(Debug, Clone)]
pub struct ServerOptions {
    pub scrcpy_version: String,
    pub max_size: u32,
    pub max_fps: u32,
    pub video_bit_rate: u32,
    pub video: bool,
    pub audio: bool,
    pub control: bool,
    /// Nombre del socket localabstract (scid).
    pub scid: String,
}

impl Default for ServerOptions {
    fn default() -> Self {
        Self {
            scrcpy_version: "5.0.1".to_string(),
            max_size: 0, // 0 = nativo
            max_fps: 60,
            video_bit_rate: 8_000_000,
            video: true,
            audio: false,
            control: true,
            scid: "0000002a".to_string(),
        }
    }
}

/// Proceso del scrcpy-server corriendo en el dispositivo.
pub struct ScrcpyServer {
    child: Child,
    serial: String,
    scid: String,
}

impl ScrcpyServer {
    /// Empuja el jar y arranca el server. `jar_path` es el jar local (vendor/).
    pub fn start(
        adb: &AdbClient,
        serial: &str,
        jar_path: &PathBuf,
        opts: &ServerOptions,
    ) -> Result<Self> {
        if !jar_path.exists() {
            return Err(TransportError::ServerMissing(jar_path.display().to_string()));
        }
        adb.push(serial, &jar_path.display().to_string(), REMOTE_JAR)?;

        // app_process arranca el server; las flags siguen el formato key=value de scrcpy 2.x.
        let cmd = format!(
            "CLASSPATH={REMOTE_JAR} app_process / {SERVER_CLASS} {ver} \
             scid={scid} tunnel_forward=false control={control} \
             video={video} audio={audio} max_size={max_size} max_fps={max_fps} \
             video_bit_rate={bitrate} send_device_meta=false send_stream_meta=false \
             send_dummy_byte=false log_level=info cleanup=true{audio_extra}",
            ver = opts.scrcpy_version,
            scid = opts.scid,
            control = opts.control,
            video = opts.video,
            audio = opts.audio,
            max_size = opts.max_size,
            max_fps = opts.max_fps,
            bitrate = opts.video_bit_rate,
            // PCM crudo: sin decodificar del lado PC y sin latencia de códec.
            audio_extra = if opts.audio { " audio_codec=raw" } else { "" },
        );

        let mut command = Command::new(super::adb_exe());
        command
            .args(["-s", serial, "shell", &cmd])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        super::hide_window(&mut command);
        let mut child = command
            .spawn()
            .map_err(|e| TransportError::AdbNotFound(e.to_string()))?;

        // Volcar la salida del server a un log (diagnóstico) y no dejar los pipes llenarse:
        // si nadie los lee, adb se bloquea y con él el server del dispositivo.
        let log_path = server_log_path(serial);
        if let Some(out) = child.stdout.take() {
            spawn_log_copy(out, log_path.clone());
        }
        if let Some(err) = child.stderr.take() {
            spawn_log_copy(err, log_path);
        }

        Ok(Self { child, serial: serial.to_string(), scid: opts.scid.clone() })
    }

    pub fn scid(&self) -> &str {
        &self.scid
    }
}

/// Ruta del log del server para un dispositivo: `%TEMP%/Aguara/server-<serial>.log`.
pub fn server_log_path(serial: &str) -> PathBuf {
    let dir = std::env::temp_dir().join("Aguara");
    let _ = std::fs::create_dir_all(&dir);
    let safe: String = serial
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    dir.join(format!("server-{safe}.log"))
}

fn spawn_log_copy<R: std::io::Read + Send + 'static>(mut src: R, path: PathBuf) {
    std::thread::spawn(move || {
        let file = std::fs::OpenOptions::new().create(true).append(true).open(&path);
        match file {
            Ok(mut f) => {
                let _ = std::io::copy(&mut src, &mut f);
            }
            Err(_) => {
                let _ = std::io::copy(&mut src, &mut std::io::sink());
            }
        }
    });
}

impl Drop for ScrcpyServer {
    fn drop(&mut self) {
        // Propiedad del proceso: matar al salir (cleanup=true también ayuda del lado device).
        let _ = self.child.kill();
        let _ = self.child.wait();
        // El reverse se limpia desde SessionManager con el AdbClient (ver app/).
        let _ = &self.serial;
    }
}
