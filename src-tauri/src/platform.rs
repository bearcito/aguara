//! Diferencias entre sistemas (Windows, macOS, Linux): dónde viven las herramientas externas
//! (adb, ffmpeg, ffplay), cómo se abre una carpeta y cómo se lee el portapapeles.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// Nombre del ejecutable según el sistema (`adb` -> `adb.exe` en Windows).
pub fn exe_name(tool: &str) -> String {
    if cfg!(windows) {
        format!("{tool}.exe")
    } else {
        tool.to_string()
    }
}

/// Carpeta del usuario.
pub fn home_dir() -> Option<PathBuf> {
    let var = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
    std::env::var_os(var).filter(|v| !v.is_empty()).map(PathBuf::from)
}

/// Carpeta donde está el ejecutable de la app.
fn exe_dir() -> Option<PathBuf> {
    Some(std::env::current_exe().ok()?.parent()?.to_path_buf())
}

/// Carpeta donde la app descarga herramientas (adb, ffmpeg).
///
/// En Windows es la carpeta de la app (la instalación es por usuario y se puede escribir). En
/// macOS la app es un paquete `.app` que no hay que modificar (rompe la firma y puede ser de
/// solo lectura), así que van a `~/Library/Application Support/Aguara`; en Linux, a
/// `~/.local/share/aguara`.
pub fn tools_dir() -> Option<PathBuf> {
    if cfg!(windows) {
        exe_dir()
    } else if cfg!(target_os = "macos") {
        Some(home_dir()?.join("Library").join("Application Support").join("Aguara"))
    } else {
        let base = std::env::var_os("XDG_DATA_HOME")
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
            .or_else(|| home_dir().map(|h| h.join(".local").join("share")))?;
        Some(base.join("aguara"))
    }
}

/// Carpetas donde buscar una herramienta, de la más propia a la más general.
fn tool_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(exe) = exe_dir() {
        dirs.push(exe.clone());
        dirs.push(exe.join("platform-tools"));
        // macOS: los recursos del paquete están en `Aguará.app/Contents/Resources`.
        if let Some(contents) = exe.parent() {
            dirs.push(contents.join("Resources"));
        }
    }
    if let Some(tools) = tools_dir() {
        dirs.push(tools.clone());
        dirs.push(tools.join("platform-tools"));
    }
    if cfg!(unix) {
        // Una app abierta desde el Finder no hereda el PATH de la terminal: Homebrew, MacPorts
        // y el SDK de Android quedan afuera si no se buscan a mano.
        for d in ["/opt/homebrew/bin", "/usr/local/bin", "/opt/local/bin", "/usr/bin"] {
            dirs.push(PathBuf::from(d));
        }
        if let Some(home) = home_dir() {
            dirs.push(home.join("Library/Android/sdk/platform-tools"));
            dirs.push(home.join("Android/Sdk/platform-tools"));
        }
    }
    dirs
}

/// Ruta de una herramienta externa, si está en alguna de las carpetas conocidas.
pub fn find_tool(tool: &str) -> Option<PathBuf> {
    let name = exe_name(tool);
    tool_dirs().into_iter().map(|d| d.join(&name)).find(|p| p.is_file())
}

/// Como `find_tool`, pero si no está devuelve el nombre solo (se busca en el PATH al ejecutar).
pub fn tool_or_path(tool: &str) -> PathBuf {
    find_tool(tool).unwrap_or_else(|| PathBuf::from(exe_name(tool)))
}

/// La herramienta está en una carpeta conocida o responde en el PATH.
pub fn tool_available(tool: &str) -> bool {
    if find_tool(tool).is_some() {
        return true;
    }
    let mut probe = Command::new(exe_name(tool));
    probe.arg("-version").stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    crate::transport::hide_window(&mut probe);
    probe.status().is_ok_and(|s| s.success())
}

/// Deja un archivo descargado listo para ejecutar (en Unix, con permiso de ejecución).
pub fn make_executable(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Ok(meta) = std::fs::metadata(path) {
            let mut perms = meta.permissions();
            perms.set_mode(perms.mode() | 0o755);
            let _ = std::fs::set_permissions(path, perms);
        }
    }
    #[cfg(not(unix))]
    let _ = path;
}

/// Ruta escrita por el usuario -> ruta real: saca las comillas que agrega "Copiar como ruta" y
/// expande `~/` (adb no lo hace).
pub fn user_path(raw: &str) -> String {
    let p = raw.trim();
    let p = p
        .strip_prefix('"')
        .and_then(|s| s.strip_suffix('"'))
        .or_else(|| p.strip_prefix('\'').and_then(|s| s.strip_suffix('\'')))
        .unwrap_or(p);
    if let Some(rest) = p.strip_prefix("~/") {
        if let Some(home) = home_dir() {
            return home.join(rest).display().to_string();
        }
    }
    p.to_string()
}

/// Abre una carpeta en el explorador de archivos del sistema.
pub fn open_folder(dir: &Path) -> std::io::Result<()> {
    let program = if cfg!(windows) {
        "explorer"
    } else if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };
    Command::new(program)
        .arg(dir)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map(|_| ())
}

/// Texto del portapapeles de la computadora.
pub fn read_clipboard() -> Result<String, String> {
    let run = |mut cmd: Command| -> Result<String, String> {
        cmd.stdin(Stdio::null());
        crate::transport::hide_window(&mut cmd);
        let out = cmd.output().map_err(|e| e.to_string())?;
        if !out.status.success() {
            return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
        }
        Ok(String::from_utf8_lossy(&out.stdout).trim_end_matches(['\r', '\n']).to_string())
    };
    if cfg!(windows) {
        let mut cmd = Command::new("powershell");
        cmd.args([
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            "[Console]::OutputEncoding=[Text.Encoding]::UTF8; Get-Clipboard -Raw",
        ]);
        run(cmd)
    } else if cfg!(target_os = "macos") {
        // Sin una configuración regional UTF-8 (una app abierta desde el Finder no la tiene),
        // pbpaste cambia acentos y emojis por "?".
        let mut cmd = Command::new("/usr/bin/pbpaste");
        cmd.env("LC_CTYPE", "UTF-8");
        run(cmd)
    } else {
        // Linux: Wayland o X11, lo que haya.
        let candidates: [(&str, &[&str]); 3] = [
            ("wl-paste", &["--no-newline"]),
            ("xclip", &["-selection", "clipboard", "-o"]),
            ("xsel", &["--clipboard", "--output"]),
        ];
        let mut last = "no hay una herramienta de portapapeles (wl-paste, xclip o xsel)".to_string();
        for (program, args) in candidates {
            let mut cmd = Command::new(program);
            cmd.args(args);
            match run(cmd) {
                Ok(text) => return Ok(text),
                Err(e) if !e.is_empty() => last = e,
                Err(_) => {}
            }
        }
        Err(last)
    }
}

#[cfg(test)]
mod tests {
    use super::user_path;

    #[test]
    fn user_path_strips_quotes_and_spaces() {
        assert_eq!(user_path("  \"C:\\apps\\mi app.apk\" "), "C:\\apps\\mi app.apk");
        assert_eq!(user_path("'/tmp/a b.png'"), "/tmp/a b.png");
        assert_eq!(user_path("/tmp/app.apk"), "/tmp/app.apk");
    }

    #[test]
    fn user_path_expands_home() {
        let Some(home) = super::home_dir() else { return };
        assert_eq!(user_path("~/Downloads/app.apk"), home.join("Downloads/app.apk").display().to_string());
        // Un "~" en medio de la ruta no se toca.
        assert_eq!(user_path("/tmp/~/x"), "/tmp/~/x");
    }
}
