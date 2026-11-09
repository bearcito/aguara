//! Capa de transporte: ADB, ciclo de vida del scrcpy-server y la conexión de sockets.

pub mod adb;
pub mod connection;
pub mod server;

pub use adb::{AdbClient, DeviceInfo};
pub use connection::DeviceConnection;
pub use server::{ServerOptions, ScrcpyServer};

use thiserror::Error;

/// Errores de transporte (álgebra tipada; se mapea a texto en la UI).
#[derive(Debug, Error)]
pub enum TransportError {
    #[error("adb no encontrado o no ejecutable: {0}")]
    AdbNotFound(String),
    #[error("comando adb falló ({code}): {stderr}")]
    AdbCommand { code: i32, stderr: String },
    #[error("no hay dispositivos autorizados")]
    NoDevice,
    #[error("scrcpy-server no encontrado en {0}")]
    ServerMissing(String),
    #[error("handshake del socket falló: {0}")]
    Handshake(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

pub type Result<T> = std::result::Result<T, TransportError>;

/// En Windows, evita que los subprocesos (adb, app_process) abran una ventana de consola.
/// En el resto de plataformas es un no-op.
#[cfg(windows)]
pub fn hide_window(cmd: &mut std::process::Command) {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    cmd.creation_flags(CREATE_NO_WINDOW);
}

#[cfg(not(windows))]
pub fn hide_window(_cmd: &mut std::process::Command) {}

/// Ejecutable de adb. Si ya hay un server de adb corriendo (por ejemplo el de Panda), se usa ese
/// mismo programa: un adb de otra versión mata al server que encuentra y arranca el suyo, y si
/// otra app hace lo mismo, los teléfonos se desconectan y reconectan sin parar. Si no hay
/// ninguno, el que la app descargó junto a sí misma (`platform-tools/`) o el del PATH.
pub fn adb_exe() -> std::path::PathBuf {
    if let Some(p) = running_server_adb() {
        return p;
    }
    match local_adb() {
        Some(p) if p.exists() => p,
        _ => std::path::PathBuf::from("adb"),
    }
}

/// Programa del server de adb que está corriendo, si hay uno. Se revisa cada pocos segundos
/// (recorrer los procesos en cada comando sería caro).
fn running_server_adb() -> Option<std::path::PathBuf> {
    use std::sync::Mutex;
    use std::time::{Duration, Instant};
    static CACHE: Mutex<Option<(Instant, Option<std::path::PathBuf>)>> = Mutex::new(None);
    let mut cache = CACHE.lock().ok()?;
    if let Some((at, found)) = cache.as_ref() {
        if at.elapsed() < Duration::from_secs(4) {
            return found.clone();
        }
    }
    let found = find_adb_server();
    *cache = Some((Instant::now(), found.clone()));
    found
}

fn find_adb_server() -> Option<std::path::PathBuf> {
    use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};
    let mut sys = System::new();
    sys.refresh_processes_specifics(
        ProcessesToUpdate::All,
        true,
        ProcessRefreshKind::nothing().with_exe(UpdateKind::Always).with_cmd(UpdateKind::Always),
    );
    sys.processes()
        .values()
        .filter(|p| p.name().to_string_lossy().to_ascii_lowercase().starts_with("adb"))
        // El server se lanza como "adb ... server" o "adb ... fork-server server".
        .filter(|p| p.cmd().iter().any(|a| a == "server" || a == "fork-server"))
        .find_map(|p| p.exe().filter(|e| e.exists()).map(|e| e.to_path_buf()))
}

/// Otro server de adb abierto aparte del normal (puerto 5037): de otra app o de una sesión vieja.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ForeignAdb {
    pub port: u16,
    /// Teléfonos que tiene tomados.
    pub devices: usize,
}

/// Servers de adb que escuchan en otro puerto. Cada teléfono lo puede tomar un solo server, así
/// que uno de esos se queda con algunos teléfonos y Aguará no los ve.
pub fn foreign_adb_servers() -> Vec<ForeignAdb> {
    use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, System};
    let mut sys = System::new();
    sys.refresh_processes_specifics(ProcessesToUpdate::All, true, ProcessRefreshKind::nothing());
    let adb_pids: std::collections::HashSet<u32> = sys
        .processes()
        .iter()
        .filter(|(_, p)| p.name().to_string_lossy().to_ascii_lowercase().starts_with("adb"))
        .map(|(pid, _)| pid.as_u32())
        .collect();
    if adb_pids.is_empty() {
        return Vec::new();
    }
    let mut netstat = std::process::Command::new("netstat");
    netstat.args(["-ano", "-p", "TCP"]).stdin(std::process::Stdio::null());
    hide_window(&mut netstat);
    let Ok(out) = netstat.output() else { return Vec::new() };
    // "TCP  127.0.0.1:5038  0.0.0.0:0  LISTENING  18736"
    let mut by_pid: std::collections::HashMap<u32, Vec<u16>> = std::collections::HashMap::new();
    for line in String::from_utf8_lossy(&out.stdout).lines() {
        let cols: Vec<&str> = line.split_whitespace().collect();
        let [_, local, _, state, pid] = cols[..] else { continue };
        let (Ok(pid), Some(port)) = (pid.parse::<u32>(), local.rsplit(':').next().and_then(|p| p.parse::<u16>().ok()))
        else {
            continue;
        };
        if state == "LISTENING" && adb_pids.contains(&pid) {
            by_pid.entry(pid).or_default().push(port);
        }
    }
    by_pid
        .values()
        .filter(|ports| !ports.contains(&5037))
        // El puerto del server es bajo (5037 en adelante); los altos son túneles hacia los
        // teléfonos y no se tocan.
        .filter_map(|ports| ports.iter().copied().filter(|p| *p < 10_000).min())
        .map(|port| ForeignAdb { port, devices: count_devices(port) })
        .collect()
}

fn count_devices(port: u16) -> usize {
    let mut cmd = std::process::Command::new(adb_exe());
    cmd.args(["-P", &port.to_string(), "devices"]).stdin(std::process::Stdio::null());
    hide_window(&mut cmd);
    cmd.output()
        .map(|o| {
            String::from_utf8_lossy(&o.stdout)
                .lines()
                .skip(1)
                .filter(|l| l.split_whitespace().nth(1).is_some())
                .count()
        })
        .unwrap_or(0)
}

/// Le pide a ese server de adb que se cierre (por el mismo protocolo de adb). Los teléfonos que
/// tenía quedan libres para el server normal.
pub fn stop_adb_server(port: u16) -> std::result::Result<(), String> {
    if port == 5037 {
        return Err("ese es el adb normal".into());
    }
    let mut cmd = std::process::Command::new(adb_exe());
    cmd.args(["-P", &port.to_string(), "kill-server"]).stdin(std::process::Stdio::null());
    hide_window(&mut cmd);
    cmd.output().map(|_| ()).map_err(|e| e.to_string())
}

fn local_adb() -> Option<std::path::PathBuf> {
    let dir = std::env::current_exe().ok()?.parent()?.to_path_buf();
    Some(dir.join("platform-tools").join(if cfg!(windows) { "adb.exe" } else { "adb" }))
}

/// Si no hay adb usable, descarga las platform-tools oficiales de Google junto a la app (no se
/// redistribuyen en el instalador). Usa `curl` y `tar`, que vienen con Windows 10/11.
pub fn ensure_adb() -> std::result::Result<(), String> {
    let mut probe = std::process::Command::new(adb_exe());
    probe.arg("version").stdin(std::process::Stdio::null());
    hide_window(&mut probe);
    if probe.output().is_ok_and(|o| o.status.success()) {
        return Ok(());
    }
    let dest = local_adb()
        .and_then(|p| p.parent()?.parent().map(|d| d.to_path_buf()))
        .ok_or("no se encontró la carpeta de la app")?;
    let zip = std::env::temp_dir().join("aguara-platform-tools.zip");
    let url = "https://dl.google.com/android/repository/platform-tools-latest-windows.zip";
    let run = |mut cmd: std::process::Command| -> std::result::Result<(), String> {
        hide_window(&mut cmd);
        let out = cmd.output().map_err(|e| e.to_string())?;
        if out.status.success() {
            Ok(())
        } else {
            Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
        }
    };
    let mut curl = std::process::Command::new("curl");
    curl.args(["-sSfL", "-o"]).arg(&zip).arg(url);
    run(curl).map_err(|e| format!("no se pudo descargar adb: {e}"))?;
    let mut tar = std::process::Command::new("tar");
    tar.arg("-xf").arg(&zip).arg("-C").arg(&dest);
    let r = run(tar).map_err(|e| format!("no se pudo descomprimir adb: {e}"));
    let _ = std::fs::remove_file(&zip);
    r
}
