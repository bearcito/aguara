//! Capa de aplicación: una Session por dispositivo y el manejador que las posee.
//!
//! Varias sesiones a la vez (grilla). El arranque (adb reverse + server + accept) es lento y
//! puede fallar, así que corre SIN tener el lock de sesiones: se arrancan en paralelo y un
//! dispositivo colgado no traba a los demás.

use std::collections::{HashMap, HashSet};
use std::io::Write;
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::process::Child;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use crate::domain::ControlMessage;
use crate::media::{self, Frame};
use crate::transport::{AdbClient, DeviceConnection, ScrcpyServer, ServerOptions, TransportError};

/// Callback de frames (lo conecta commands.rs a un evento Tauri).
pub type FrameSink = Arc<dyn Fn(&str, Frame) + Send + Sync>;

/// Callback de fin de sesión (desconexión no solicitada) -> evento Tauri.
pub type EndSink = Arc<dyn Fn(&str) + Send + Sync>;

/// Hash FNV-1a del serial, enmascarado a 31 bits (scid válido para scrcpy, != -1).
fn scid_from(serial: &str) -> u32 {
    let mut hash: u32 = 0x811c9dc5;
    for b in serial.bytes() {
        hash ^= b as u32;
        hash = hash.wrapping_mul(0x0100_0193);
    }
    hash & 0x7fff_ffff
}

/// Último frame JPEG (para automación por imagen; se decodifica solo al buscar).
type LastFrame = Arc<Mutex<Option<Vec<u8>>>>;

/// Parámetros de un stream (todos configurables desde la UI).
#[derive(Debug, Clone, Copy)]
pub struct StreamParams {
    /// Lado mayor del video en px (0 = nativo).
    pub max_size: u32,
    pub max_fps: u32,
    /// Bitrate H.264 en bps.
    pub bit_rate: u32,
    /// Calidad JPEG hacia la UI (2 = mejor, 31 = peor).
    pub jpeg_quality: u8,
}

struct Session {
    _server: ScrcpyServer,
    running: Arc<AtomicBool>,
    scid: String,
    /// Socket de control (PC -> teléfono). None si la sesión se abrió sin control.
    control: Option<Arc<Mutex<TcpStream>>>,
    last_frame: LastFrame,
}

/// Audio de un teléfono sonando en la PC: un segundo server (solo audio) + ffplay.
struct AudioSession {
    _server: ScrcpyServer,
    player: Child,
    scid: String,
}

impl Drop for AudioSession {
    fn drop(&mut self) {
        let _ = self.player.kill();
        let _ = self.player.wait();
    }
}

/// Posee todas las sesiones vivas y su limpieza.
pub struct SessionManager {
    adb: AdbClient,
    jar_path: PathBuf,
    sessions: Mutex<HashMap<String, Session>>,
    audio: Mutex<HashMap<String, AudioSession>>,
    /// Dispositivos con un arranque en curso (evita sesiones duplicadas).
    starting: Mutex<HashSet<String>>,
    sink: FrameSink,
    end_sink: EndSink,
}

impl SessionManager {
    pub fn new(adb: AdbClient, jar_path: PathBuf, sink: FrameSink, end_sink: EndSink) -> Self {
        Self {
            adb,
            jar_path,
            sessions: Mutex::new(HashMap::new()),
            audio: Mutex::new(HashMap::new()),
            starting: Mutex::new(HashSet::new()),
            sink,
            end_sink,
        }
    }

    pub fn adb(&self) -> &AdbClient {
        &self.adb
    }

    /// Serials con sesión activa.
    pub fn active(&self) -> Vec<String> {
        self.sessions.lock().unwrap().keys().cloned().collect()
    }

    /// Arranca el mirroring de un dispositivo. Bloquea hasta que el video conecta (o falla).
    pub fn start(&self, serial: &str, params: StreamParams) -> crate::transport::Result<()> {
        if self.sessions.lock().unwrap().contains_key(serial) {
            return Ok(());
        }
        if !self.starting.lock().unwrap().insert(serial.to_string()) {
            return Ok(()); // ya se está arrancando
        }
        let result = self.start_inner(serial, params);
        self.starting.lock().unwrap().remove(serial);
        result
    }

    fn start_inner(&self, serial: &str, params: StreamParams) -> crate::transport::Result<()> {
        let mut opts = ServerOptions::default();
        opts.max_size = params.max_size;
        if params.max_fps > 0 {
            opts.max_fps = params.max_fps;
        }
        if params.bit_rate > 0 {
            opts.video_bit_rate = params.bit_rate;
        }
        // scid único por dispositivo (hex de 31 bits, como espera scrcpy) para permitir
        // varias sesiones a la vez sin colisión de sockets.
        opts.scid = format!("{:08x}", scid_from(serial));
        let socket_name = format!("scrcpy_{}", opts.scid);

        // Puerto efímero local; el server del device conecta de vuelta (tunnel_forward=false).
        let listener = TcpListener::bind("127.0.0.1:0")?;
        let port = listener.local_addr()?.port();
        self.adb.reverse(serial, &socket_name, port)?;

        let server = match ScrcpyServer::start(&self.adb, serial, &self.jar_path, &opts) {
            Ok(s) => s,
            Err(e) => {
                let _ = self.adb.reverse_remove(serial, &socket_name);
                return Err(e);
            }
        };
        let conn = match DeviceConnection::accept(&listener, opts.control) {
            Ok(c) => c,
            Err(e) => {
                let _ = self.adb.reverse_remove(serial, &socket_name);
                let log = crate::transport::server::server_log_path(serial);
                return Err(TransportError::Handshake(format!("{e} — ver {}", log.display())));
            }
        };

        let running = Arc::new(AtomicBool::new(true));
        // El server manda por el socket de control los cambios del portapapeles del teléfono;
        // leerlos y descartarlos para que su buffer nunca se llene.
        if let Some(mut rx) = conn.control.as_ref().and_then(|s| s.try_clone().ok()) {
            std::thread::spawn(move || {
                let _ = std::io::copy(&mut rx, &mut std::io::sink());
            });
        }
        let control = conn.control.map(|s| Arc::new(Mutex::new(s)));
        let last_frame: LastFrame = Arc::new(Mutex::new(None));

        {
            let sink = self.sink.clone();
            let end = self.end_sink.clone();
            let flag = running.clone();
            let last = last_frame.clone();
            let serial_owned = serial.to_string();
            let video = conn.video;
            let quality = params.jpeg_quality;
            std::thread::spawn(move || {
                let _ = media::decode_h264_to_jpeg(video, quality, |frame| {
                    if !flag.load(Ordering::Relaxed) {
                        return;
                    }
                    if let Ok(mut slot) = last.lock() {
                        *slot = Some(frame.jpeg.clone());
                    }
                    sink(&serial_owned, frame);
                });
                // Si el stream terminó sin que lo detuviéramos nosotros, fue una desconexión.
                if flag.load(Ordering::Relaxed) {
                    end(&serial_owned);
                }
            });
        }

        self.sessions.lock().unwrap().insert(
            serial.to_string(),
            Session { _server: server, running, scid: opts.scid, control, last_frame },
        );
        Ok(())
    }

    /// Envía un `ControlMessage` al dispositivo por el socket de control.
    pub fn send_control(&self, serial: &str, msg: ControlMessage) -> crate::transport::Result<()> {
        let control = {
            let sessions = self.sessions.lock().unwrap();
            let session = sessions.get(serial).ok_or(TransportError::NoDevice)?;
            session
                .control
                .clone()
                .ok_or_else(|| TransportError::Handshake("sesión sin socket de control".into()))?
        };
        let bytes = msg.encode();
        let mut guard = control.lock().unwrap();
        guard.write_all(&bytes).map_err(TransportError::Io)?;
        guard.flush().ok();
        Ok(())
    }

    /// Busca una plantilla en el último frame del dispositivo. Devuelve (x, y, score) en
    /// coords del frame, o None si no supera el umbral o no hay frame todavía.
    pub fn find_template(
        &self,
        serial: &str,
        template_path: &str,
        threshold: f32,
    ) -> Option<(i32, i32, f32)> {
        // Clonar el Arc del buffer y soltar el lock de sesiones antes de hacer el match.
        let last = {
            let sessions = self.sessions.lock().unwrap();
            sessions.get(serial)?.last_frame.clone()
        };
        let jpeg = last.lock().ok()?.clone()?;
        let rgba = image::load_from_memory(&jpeg).ok()?.to_rgba8();
        let (w, h) = rgba.dimensions();
        match media::match_template(rgba.as_raw(), w, h, template_path, threshold) {
            Ok(Some(m)) => Some((m.x, m.y, m.score)),
            _ => None,
        }
    }

    /// Serials cuyo audio está sonando en la PC.
    pub fn audio_active(&self) -> Vec<String> {
        let mut map = self.audio.lock().unwrap();
        // Si ffplay terminó solo (teléfono desconectado), la sesión ya no suena.
        map.retain(|_, a| matches!(a.player.try_wait(), Ok(None)));
        map.keys().cloned().collect()
    }

    /// Empieza a reproducir en la PC el audio del teléfono (Android 11+). Mientras suena en
    /// la PC, el teléfono no lo reproduce por su parlante.
    pub fn start_audio(&self, serial: &str) -> crate::transport::Result<()> {
        if self.audio_active().iter().any(|s| s == serial) {
            return Ok(());
        }
        let mut opts = ServerOptions::default();
        opts.video = false;
        opts.control = false;
        opts.audio = true;
        // Otro scid que el del video: son dos servers independientes en el mismo teléfono.
        opts.scid = format!("{:08x}", scid_from(serial) ^ 0x4000_0000);
        let socket_name = format!("scrcpy_{}", opts.scid);

        let listener = TcpListener::bind("127.0.0.1:0")?;
        let port = listener.local_addr()?.port();
        self.adb.reverse(serial, &socket_name, port)?;
        let result = (|| {
            let server = ScrcpyServer::start(&self.adb, serial, &self.jar_path, &opts)?;
            let stream = crate::transport::connection::accept_one(&listener, "audio")?;
            let player = media::play_raw_audio(stream)?;
            Ok::<_, TransportError>(AudioSession { _server: server, player, scid: opts.scid.clone() })
        })();
        match result {
            Ok(session) => {
                self.audio.lock().unwrap().insert(serial.to_string(), session);
                Ok(())
            }
            Err(e) => {
                let _ = self.adb.reverse_remove(serial, &socket_name);
                Err(e)
            }
        }
    }

    /// Corta el audio de un teléfono (vuelve a sonar en su parlante).
    pub fn stop_audio(&self, serial: &str) {
        let removed = self.audio.lock().unwrap().remove(serial);
        if let Some(a) = removed {
            let _ = self.adb.reverse_remove(serial, &format!("scrcpy_{}", a.scid));
        }
    }

    /// Detiene y limpia la sesión de un dispositivo.
    pub fn stop(&self, serial: &str) {
        let removed = self.sessions.lock().unwrap().remove(serial);
        if let Some(session) = removed {
            session.running.store(false, Ordering::Relaxed);
            let _ = self
                .adb
                .reverse_remove(serial, &format!("scrcpy_{}", session.scid));
            // El Drop de ScrcpyServer mata el proceso del device.
        }
    }
}
