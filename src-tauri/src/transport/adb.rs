//! Cliente adb mínimo: envuelve el ejecutable `adb` (sin reimplementar el protocolo adb).

use std::path::PathBuf;
use std::process::{Command, Stdio};

use serde::Serialize;

use super::{Result, TransportError};

#[derive(Debug, Clone, Serialize)]
pub struct DeviceInfo {
    pub serial: String,
    pub state: String,
    pub model: Option<String>,
}

#[derive(Debug, Clone)]
pub struct AdbClient {
    /// Ruta al ejecutable adb (embebido o del PATH).
    adb: PathBuf,
}

impl AdbClient {
    pub fn new(adb: impl Into<PathBuf>) -> Self {
        Self { adb: adb.into() }
    }

    /// Usa el adb de la app o el del PATH (se resuelve en cada llamada: puede descargarse
    /// mientras la app ya está abierta).
    pub fn from_path() -> Self {
        Self::new("adb")
    }

    fn exe(&self) -> PathBuf {
        if self.adb.as_os_str() == "adb" {
            super::adb_exe()
        } else {
            self.adb.clone()
        }
    }

    fn run(&self, args: &[&str]) -> Result<String> {
        let mut cmd = Command::new(self.exe());
        cmd.args(args).stdin(Stdio::null());
        super::hide_window(&mut cmd);
        let output = cmd
            .output()
            .map_err(|e| TransportError::AdbNotFound(e.to_string()))?;
        if !output.status.success() {
            return Err(TransportError::AdbCommand {
                code: output.status.code().unwrap_or(-1),
                stderr: String::from_utf8_lossy(&output.stderr).trim().to_string(),
            });
        }
        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    }

    fn run_serial(&self, serial: &str, args: &[&str]) -> Result<String> {
        let mut full = vec!["-s", serial];
        full.extend_from_slice(args);
        self.run(&full)
    }

    /// Igual que `run` pero devuelve stdout crudo (para datos binarios, p. ej. screencap).
    fn run_raw(&self, args: &[&str]) -> Result<Vec<u8>> {
        let mut cmd = Command::new(self.exe());
        cmd.args(args).stdin(Stdio::null());
        super::hide_window(&mut cmd);
        let output = cmd
            .output()
            .map_err(|e| TransportError::AdbNotFound(e.to_string()))?;
        if !output.status.success() {
            return Err(TransportError::AdbCommand {
                code: output.status.code().unwrap_or(-1),
                stderr: String::from_utf8_lossy(&output.stderr).trim().to_string(),
            });
        }
        Ok(output.stdout)
    }

    /// `adb devices -l` -> lista de dispositivos.
    pub fn list_devices(&self) -> Result<Vec<DeviceInfo>> {
        let out = self.run(&["devices", "-l"])?;
        let mut devices = Vec::new();
        for line in out.lines().skip(1) {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let mut parts = line.split_whitespace();
            let serial = match parts.next() {
                Some(s) => s.to_string(),
                None => continue,
            };
            let state = parts.next().unwrap_or("unknown").to_string();
            let model = parts
                .find_map(|p| p.strip_prefix("model:"))
                .map(|m| m.replace('_', " "));
            devices.push(DeviceInfo { serial, state, model });
        }
        Ok(devices)
    }

    /// `adb -s <serial> push <local> <remote>`.
    pub fn push(&self, serial: &str, local: &str, remote: &str) -> Result<()> {
        self.run_serial(serial, &["push", local, remote]).map(|_| ())
    }

    /// `adb -s <serial> reverse localabstract:<name> tcp:<port>`.
    pub fn reverse(&self, serial: &str, name: &str, port: u16) -> Result<()> {
        let local = format!("localabstract:{name}");
        let remote = format!("tcp:{port}");
        self.run_serial(serial, &["reverse", &local, &remote]).map(|_| ())
    }

    /// `adb -s <serial> reverse --remove localabstract:<name>`.
    pub fn reverse_remove(&self, serial: &str, name: &str) -> Result<()> {
        let local = format!("localabstract:{name}");
        self.run_serial(serial, &["reverse", "--remove", &local]).map(|_| ())
    }

    /// `adb -s <serial> install -r <apk>` (reinstala conservando datos).
    pub fn install(&self, serial: &str, apk: &str) -> Result<()> {
        self.run_serial(serial, &["install", "-r", apk]).map(|_| ())
    }

    /// `adb -s <serial> shell <script>`: corre un script de shell en el dispositivo.
    pub fn shell(&self, serial: &str, script: &str) -> Result<String> {
        self.run_serial(serial, &["shell", script])
    }

    /// `adb -s <serial> reboot`.
    pub fn reboot(&self, serial: &str) -> Result<()> {
        self.run_serial(serial, &["reboot"]).map(|_| ())
    }

    /// Captura la pantalla a un PNG local (`adb exec-out screencap -p`).
    pub fn screencap(&self, serial: &str, out_path: &str) -> Result<()> {
        let png = self.run_raw(&["-s", serial, "exec-out", "screencap", "-p"])?;
        std::fs::write(out_path, png).map_err(TransportError::Io)
    }
}
