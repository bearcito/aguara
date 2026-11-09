//! Conexión de sockets con el dispositivo (video / control).
//!
//! Con `tunnel_forward=false` el server del dispositivo se conecta al PC, así que el PC
//! **escucha** en el puerto reverseado. Sin byte "dummy" ni metadatos de dispositivo/códec:
//! el socket de video trae solo paquetes `[pts+flags: u64][tamaño: u32][H.264]` (ver
//! `media::decode_h264_to_jpeg`). Orden de conexión: video, [audio], control.

use std::net::{TcpListener, TcpStream};
use std::time::{Duration, Instant};

use super::{Result, TransportError};

/// Tiempo máximo esperando que el server del dispositivo conecte cada socket.
const ACCEPT_TIMEOUT: Duration = Duration::from_secs(15);

pub struct DeviceConnection {
    pub video: TcpStream,
    pub control: Option<TcpStream>,
}

impl DeviceConnection {
    /// Acepta las conexiones entrantes del server sobre el puerto local reverseado.
    pub fn accept(listener: &TcpListener, with_control: bool) -> Result<Self> {
        let video = accept_one(listener, "video")?;
        let control = if with_control {
            Some(accept_one(listener, "control")?)
        } else {
            None
        };
        Ok(Self { video, control })
    }
}

/// `accept` con timeout: si el server falla en el dispositivo, no colgarse para siempre.
pub fn accept_one(listener: &TcpListener, what: &str) -> Result<TcpStream> {
    listener.set_nonblocking(true).map_err(TransportError::Io)?;
    let deadline = Instant::now() + ACCEPT_TIMEOUT;
    loop {
        match listener.accept() {
            Ok((stream, _addr)) => {
                stream.set_nonblocking(false).map_err(TransportError::Io)?;
                stream.set_nodelay(true).ok();
                return Ok(stream);
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                if Instant::now() >= deadline {
                    return Err(TransportError::Handshake(format!(
                        "el dispositivo no abrió el socket de {what} (timeout)"
                    )));
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(e) => return Err(TransportError::Io(e)),
        }
    }
}
