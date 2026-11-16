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
/// ninguno, el que la app descargó (`platform-tools/`), uno instalado en las carpetas usuales
/// (Homebrew, SDK de Android) o el del PATH.
pub fn adb_exe() -> std::path::PathBuf {
    if let Some(p) = running_server_adb() {
        return p;
    }
    crate::platform::tool_or_path("adb")
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
    let by_pid = listening_ports(&adb_pids);
    by_pid
        .values()
        .filter(|ports| !ports.contains(&5037))
        // El puerto del server es bajo (5037 en adelante); los altos son túneles hacia los
        // teléfonos y no se tocan.
        .filter_map(|ports| ports.iter().copied().filter(|p| *p < 10_000).min())
        .map(|port| ForeignAdb { port, devices: count_devices(port) })
        .collect()
}

/// Puertos TCP en escucha de cada uno de esos procesos.
#[cfg(windows)]
fn listening_ports(pids: &std::collections::HashSet<u32>) -> std::collections::HashMap<u32, Vec<u16>> {
    let mut by_pid: std::collections::HashMap<u32, Vec<u16>> = std::collections::HashMap::new();
    let mut netstat = std::process::Command::new("netstat");
    netstat.args(["-ano", "-p", "TCP"]).stdin(std::process::Stdio::null());
    hide_window(&mut netstat);
    let Ok(out) = netstat.output() else { return by_pid };
    // "TCP  127.0.0.1:5038  0.0.0.0:0  LISTENING  18736"
    for line in String::from_utf8_lossy(&out.stdout).lines() {
        let cols: Vec<&str> = line.split_whitespace().collect();
        let [_, local, _, state, pid] = cols[..] else { continue };
        let (Ok(pid), Some(port)) = (pid.parse::<u32>(), port_of(local)) else { continue };
        if state == "LISTENING" && pids.contains(&pid) {
            by_pid.entry(pid).or_default().push(port);
        }
    }
    by_pid
}

/// Puertos TCP en escucha de cada uno de esos procesos (macOS y Linux: `lsof`).
#[cfg(not(windows))]
fn listening_ports(pids: &std::collections::HashSet<u32>) -> std::collections::HashMap<u32, Vec<u16>> {
    let lsof = ["/usr/sbin/lsof", "/usr/bin/lsof"]
        .into_iter()
        .find(|p| std::path::Path::new(p).exists())
        .unwrap_or("lsof");
    let mut cmd = std::process::Command::new(lsof);
    cmd.args(["-nP", "-iTCP", "-sTCP:LISTEN"]).stdin(std::process::Stdio::null());
    match cmd.output() {
        Ok(out) => parse_lsof(&String::from_utf8_lossy(&out.stdout), pids),
        Err(_) => Default::default(),
    }
}

/// Salida de `lsof -nP -iTCP -sTCP:LISTEN`:
/// `adb  18736 ana  7u  IPv4 0x…  0t0  TCP 127.0.0.1:5038 (LISTEN)`.
#[cfg_attr(windows, allow(dead_code))]
fn parse_lsof(out: &str, pids: &std::collections::HashSet<u32>) -> std::collections::HashMap<u32, Vec<u16>> {
    let mut by_pid: std::collections::HashMap<u32, Vec<u16>> = std::collections::HashMap::new();
    for line in out.lines() {
        let cols: Vec<&str> = line.split_whitespace().collect();
        let Some(pid) = cols.get(1).and_then(|p| p.parse::<u32>().ok()) else { continue };
        let Some(tcp) = cols.iter().position(|c| *c == "TCP") else { continue };
        let Some(port) = cols.get(tcp + 1).and_then(|addr| port_of(addr)) else { continue };
        if pids.contains(&pid) {
            let ports = by_pid.entry(pid).or_default();
            // IPv4 e IPv6 aparecen en líneas separadas con el mismo puerto.
            if !ports.contains(&port) {
                ports.push(port);
            }
        }
    }
    by_pid
}

/// `127.0.0.1:5038`, `[::1]:5038` o `*:5038` -> 5038.
fn port_of(addr: &str) -> Option<u16> {
    addr.rsplit(':').next()?.parse().ok()
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

/// Si no hay adb usable, descarga las platform-tools oficiales de Google (no se redistribuyen
/// en el instalador) a la carpeta de herramientas de la app. Usa `curl` y un descompresor que
/// ya viene con el sistema (`tar` en Windows 10/11, `unzip` en macOS y Linux).
pub fn ensure_adb() -> std::result::Result<(), String> {
    let mut probe = std::process::Command::new(adb_exe());
    probe.arg("version").stdin(std::process::Stdio::null());
    hide_window(&mut probe);
    if probe.output().is_ok_and(|o| o.status.success()) {
        return Ok(());
    }
    let dest = crate::platform::tools_dir().ok_or("no se encontró la carpeta de la app")?;
    std::fs::create_dir_all(&dest).map_err(|e| format!("no se pudo crear {}: {e}", dest.display()))?;
    let zip = std::env::temp_dir().join("aguara-platform-tools.zip");
    let system = if cfg!(windows) {
        "windows"
    } else if cfg!(target_os = "macos") {
        "darwin"
    } else {
        "linux"
    };
    let url = format!("https://dl.google.com/android/repository/platform-tools-latest-{system}.zip");
    let run = |mut cmd: std::process::Command| -> std::result::Result<(), String> {
        cmd.stdin(std::process::Stdio::null());
        hide_window(&mut cmd);
        let out = cmd.output().map_err(|e| e.to_string())?;
        if out.status.success() {
            Ok(())
        } else {
            Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
        }
    };
    let mut curl = std::process::Command::new("curl");
    curl.args(["-sSfL", "-o"]).arg(&zip).arg(&url);
    run(curl).map_err(|e| format!("no se pudo descargar adb: {e}"))?;
    let unpack = if cfg!(windows) {
        let mut tar = std::process::Command::new("tar");
        tar.arg("-xf").arg(&zip).arg("-C").arg(&dest);
        tar
    } else {
        let mut unzip = std::process::Command::new("unzip");
        unzip.arg("-oq").arg(&zip).arg("-d").arg(&dest);
        unzip
    };
    let r = run(unpack).map_err(|e| format!("no se pudo descomprimir adb: {e}"));
    let _ = std::fs::remove_file(&zip);
    r?;
    let adb = dest.join("platform-tools").join(crate::platform::exe_name("adb"));
    if !adb.is_file() {
        return Err(format!("adb no quedó en {}", adb.display()));
    }
    crate::platform::make_executable(&adb);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{parse_lsof, port_of};

    #[test]
    fn reads_ports_from_addresses() {
        assert_eq!(port_of("127.0.0.1:5038"), Some(5038));
        assert_eq!(port_of("[::1]:5037"), Some(5037));
        assert_eq!(port_of("*:7000"), Some(7000));
        assert_eq!(port_of("0.0.0.0:0"), Some(0));
        assert_eq!(port_of("NAME"), None);
    }

    #[test]
    fn parses_lsof_listeners_of_the_given_pids() {
        let out = "COMMAND   PID USER   FD   TYPE             DEVICE SIZE/OFF NODE NAME
adb     18736  ana    7u  IPv4 0x8a1f3c2d1e0b7a11      0t0  TCP 127.0.0.1:5038 (LISTEN)
adb     18736  ana    8u  IPv6 0x8a1f3c2d1e0b7a12      0t0  TCP [::1]:5038 (LISTEN)
adb     18736  ana   12u  IPv4 0x8a1f3c2d1e0b7a13      0t0  TCP 127.0.0.1:27183 (LISTEN)
adb       912  ana    7u  IPv4 0x8a1f3c2d1e0b7a14      0t0  TCP 127.0.0.1:5037 (LISTEN)
rapportd  544  ana    4u  IPv4 0x8a1f3c2d1e0b7a15      0t0  TCP *:49152 (LISTEN)
";
        let pids = [18736u32, 912].into_iter().collect();
        let by_pid = parse_lsof(out, &pids);
        assert_eq!(by_pid.get(&18736), Some(&vec![5038, 27183]));
        assert_eq!(by_pid.get(&912), Some(&vec![5037]));
        assert_eq!(by_pid.len(), 2);
    }
}
