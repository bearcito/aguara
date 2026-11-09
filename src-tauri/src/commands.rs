//! Puente Tauri: comandos invocables desde la UI y emisión de frames como eventos.

use serde::Serialize;
use tauri::{Emitter, Manager, State};

use crate::app::SessionManager;
use crate::domain::{ControlMessage, Position};
use crate::transport::DeviceInfo;

// ---------- Tareas automáticas que se pueden detener ----------
// Cada tarea larga (aceptar solicitudes, cambiar de cuenta) registra una bandera por teléfono y
// la mira entre paso y paso; "Detener tareas" la levanta.
type StopFlag = std::sync::Arc<std::sync::atomic::AtomicBool>;
static TASKS: std::sync::LazyLock<std::sync::Mutex<std::collections::HashMap<String, StopFlag>>> =
    std::sync::LazyLock::new(Default::default);

fn start_task(serial: &str) -> StopFlag {
    let flag = StopFlag::default();
    if let Ok(mut tasks) = TASKS.lock() {
        tasks.insert(serial.to_string(), flag.clone());
    }
    flag
}

fn end_task(serial: &str, flag: &StopFlag) {
    if let Ok(mut tasks) = TASKS.lock() {
        if tasks.get(serial).is_some_and(|f| std::sync::Arc::ptr_eq(f, flag)) {
            tasks.remove(serial);
        }
    }
}

fn stopped(flag: &StopFlag) -> bool {
    flag.load(std::sync::atomic::Ordering::Relaxed)
}

/// Espera `ms` en tramos cortos; devuelve false si mientras tanto pidieron detener.
fn wait_unless_stopped(ms: u64, flag: &StopFlag) -> bool {
    let mut left = ms;
    while left > 0 {
        if stopped(flag) {
            return false;
        }
        let step = left.min(150);
        std::thread::sleep(std::time::Duration::from_millis(step));
        left -= step;
    }
    !stopped(flag)
}

/// Detiene las tareas automáticas de esos teléfonos (o de todos, si la lista está vacía).
#[tauri::command]
pub fn stop_tasks(serials: Vec<String>) {
    if let Ok(tasks) = TASKS.lock() {
        for (serial, flag) in tasks.iter() {
            if serials.is_empty() || serials.contains(serial) {
                flag.store(true, std::sync::atomic::Ordering::Relaxed);
            }
        }
    }
}

/// Estado compartido de la app.
pub struct AppState {
    pub manager: std::sync::Arc<SessionManager>,
}

#[derive(Clone, Serialize)]
struct VideoFramePayload {
    serial: String,
    width: u32,
    height: u32,
    /// Frame JPEG en base64 (la UI lo dibuja como imagen).
    jpeg: String,
}

/// Base64 estándar (RFC 4648, con padding). Suficiente para los frames; evita una dependencia.
fn base64_encode(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for c in data.chunks(3) {
        let b = [c[0], *c.get(1).unwrap_or(&0), *c.get(2).unwrap_or(&0)];
        let n = (b[0] as u32) << 16 | (b[1] as u32) << 8 | b[2] as u32;
        out.push(T[(n >> 18 & 63) as usize] as char);
        out.push(T[(n >> 12 & 63) as usize] as char);
        out.push(if c.len() > 1 { T[(n >> 6 & 63) as usize] as char } else { '=' });
        out.push(if c.len() > 2 { T[(n & 63) as usize] as char } else { '=' });
    }
    out
}

#[tauri::command]
pub fn list_devices(state: State<AppState>) -> Result<Vec<DeviceInfo>, String> {
    state.manager.adb().list_devices().map_err(|e| e.to_string())
}

/// Arranca el mirroring. Async + hilo bloqueante: la UI no se congela mientras el teléfono
/// conecta, y varios dispositivos pueden arrancar en paralelo.
#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub async fn start_mirror(
    serial: String,
    max_size: Option<u32>,
    max_fps: Option<u32>,
    bit_rate: Option<u32>,
    jpeg_quality: Option<u8>,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let manager = state.manager.clone();
    let params = crate::app::StreamParams {
        max_size: max_size.unwrap_or(0),
        max_fps: max_fps.unwrap_or(0),
        bit_rate: bit_rate.unwrap_or(0),
        jpeg_quality: jpeg_quality.unwrap_or(6),
    };
    tauri::async_runtime::spawn_blocking(move || manager.start(&serial, params))
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| e.to_string())
}

/// Serials con sesión activa.
#[tauri::command]
pub fn list_active(state: State<AppState>) -> Vec<String> {
    state.manager.active()
}

#[derive(Clone, Serialize)]
pub struct MatchResult {
    pub x: i32,
    pub y: i32,
    pub score: f32,
}

/// Busca una plantilla PNG/JPG en el último frame del dispositivo (automación por imagen).
#[tauri::command]
pub fn find_template(
    serial: String,
    path: String,
    threshold: Option<f32>,
    state: State<AppState>,
) -> Option<MatchResult> {
    state
        .manager
        .find_template(&serial, &path, threshold.unwrap_or(0.8))
        .map(|(x, y, score)| MatchResult { x, y, score })
}

#[tauri::command]
pub fn stop_mirror(serial: String, state: State<AppState>) {
    state.manager.stop(&serial);
}

/// Puntero "dedo genérico" de scrcpy: Android lo recibe como pantalla táctil (deslizar en la
/// pantalla de bloqueo, scroll con inercia). Con el puntero de mouse (-1) los arrastres no
/// desbloquean ni desplazan listas como un dedo.
const POINTER_ID_GENERIC_FINGER: i64 = -2;

// Los comandos de entrada son `async`: corren fuera del hilo principal, que también entrega
// los frames de video, así un arrastre no se encola detrás del render.

/// Inyecta un evento táctil. `action`: 0=DOWN, 1=UP, 2=MOVE. `width`/`height` = tamaño del
/// frame donde se tomaron `x`/`y` (el frame mostrado).
#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub async fn inject_touch(
    serial: String,
    action: u8,
    x: i32,
    y: i32,
    width: u16,
    height: u16,
    state: State<'_, AppState>,
) -> Result<(), String> {
    // Presión 1.0 mientras toca, 0 al soltar. Sin botones: es un dedo, no un mouse.
    let pressure = if action == 1 { 0.0 } else { 1.0 };
    let msg = ControlMessage::InjectTouch {
        action,
        pointer_id: POINTER_ID_GENERIC_FINGER,
        position: Position { x, y, width, height },
        pressure,
        action_button: 0,
        buttons: 0,
    };
    state.manager.send_control(&serial, msg).map_err(|e| e.to_string())
}

/// Inyecta un keycode Android. `action`: 0=DOWN, 1=UP.
#[tauri::command]
pub async fn inject_key(
    serial: String,
    keycode: i32,
    action: u8,
    meta_state: i32,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let msg = ControlMessage::InjectKeycode { action, keycode, repeat: 0, meta_state };
    state.manager.send_control(&serial, msg).map_err(|e| e.to_string())
}

/// Inyecta un evento de scroll. `h_scroll`/`v_scroll` en [-1.0, 1.0].
#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub async fn inject_scroll(
    serial: String,
    x: i32,
    y: i32,
    width: u16,
    height: u16,
    h_scroll: f32,
    v_scroll: f32,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let msg = ControlMessage::InjectScroll {
        position: Position { x, y, width, height },
        h_scroll,
        v_scroll,
        buttons: 0,
    };
    state.manager.send_control(&serial, msg).map_err(|e| e.to_string())
}

/// Inyecta texto UTF-8 (escritura directa).
#[tauri::command]
pub async fn inject_text(
    serial: String,
    text: String,
    state: State<'_, AppState>,
) -> Result<(), String> {
    state
        .manager
        .send_control(&serial, ControlMessage::InjectText { text })
        .map_err(|e| e.to_string())
}

/// Instala un APK local en el dispositivo (reinstala conservando datos).
#[tauri::command]
pub fn install_apk(serial: String, path: String, state: State<AppState>) -> Result<(), String> {
    state.manager.adb().install(&serial, &path).map_err(|e| e.to_string())
}

/// Empuja un archivo local al dispositivo.
#[tauri::command]
pub fn push_file(
    serial: String,
    local: String,
    remote: String,
    state: State<AppState>,
) -> Result<(), String> {
    state.manager.adb().push(&serial, &local, &remote).map_err(|e| e.to_string())
}

/// Captura la pantalla del dispositivo a un PNG. Rutas relativas van a
/// `<Imágenes>/Aguará/` (escribible también con la app instalada). Devuelve la ruta final.
#[tauri::command]
pub fn screenshot(serial: String, out: String, state: State<AppState>) -> Result<String, String> {
    let mut path = std::path::PathBuf::from(&out);
    if path.is_relative() {
        // Rutas relativas (p. ej. "2026-10-29/Telefono_14-05-12.png") van dentro de la carpeta de
        // capturas; sin ".." para no salir de ella.
        if path.components().any(|c| matches!(c, std::path::Component::ParentDir)) {
            return Err("ruta de captura inválida".into());
        }
        path = shots_dir().join(path);
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    let path_str = path.display().to_string();
    state.manager.adb().screencap(&serial, &path_str).map_err(|e| e.to_string())?;
    Ok(path_str)
}

/// Ficha del dispositivo: estado, cuentas y apps (una sola llamada a adb).
#[tauri::command]
pub async fn device_details(
    serial: String,
    state: State<'_, AppState>,
) -> Result<crate::domain::DeviceDetails, String> {
    let manager = state.manager.clone();
    tauri::async_runtime::spawn_blocking(move || {
        manager
            .adb()
            .shell(&serial, crate::domain::DETAILS_SCRIPT)
            .map(|out| crate::domain::parse_details(&out))
    })
    .await
    .map_err(|e| e.to_string())?
    .map_err(|e| e.to_string())
}

/// Abre una app por su paquete (lanza su actividad principal).
#[tauri::command]
pub async fn launch_app(serial: String, package: String, state: State<'_, AppState>) -> Result<(), String> {
    // Solo nombres de paquete válidos: evita inyectar shell vía el argumento.
    if package.is_empty()
        || !package.chars().all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '_')
    {
        return Err(format!("paquete inválido: {package}"));
    }
    let manager = state.manager.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let script = format!("monkey -p {package} -c android.intent.category.LAUNCHER 1");
        manager.adb().shell(&serial, &script).map(|_| ())
    })
    .await
    .map_err(|e| e.to_string())?
    .map_err(|e| e.to_string())
}

/// Botón de pánico: borra los datos de todas las apps que instaló el usuario (y de Chrome), lo
/// que cierra todas las sesiones, y apaga la pantalla. Las apps quedan instaladas. Un
/// restablecimiento de fábrica real no se puede pedir por adb sin root. Devuelve cuántas apps
/// se borraron.
#[tauri::command]
pub async fn panic_wipe(serial: String, state: State<'_, AppState>) -> Result<u32, String> {
    let manager = state.manager.clone();
    tauri::async_runtime::spawn_blocking(move || {
        // Un solo viaje por adb: la lista de apps y el borrado corren en el teléfono.
        let script = "for p in $(pm list packages -3 | cut -d: -f2) com.android.chrome; do \
             pm clear \"$p\" >/dev/null 2>&1 && echo ok; done; input keyevent 223";
        let out = manager.adb().shell(&serial, script).map_err(|e| e.to_string())?;
        Ok(out.lines().filter(|l| l.trim() == "ok").count() as u32)
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Otros servers de adb abiertos que se quedaron con teléfonos (ver `foreign_adb_servers`).
#[tauri::command]
pub async fn foreign_adb() -> Result<Vec<crate::transport::ForeignAdb>, String> {
    tauri::async_runtime::spawn_blocking(crate::transport::foreign_adb_servers)
        .await
        .map_err(|e| e.to_string())
}

/// Cierra esos otros servers de adb para que sus teléfonos aparezcan en Aguará.
#[tauri::command]
pub async fn free_adb(ports: Vec<u16>) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || {
        for port in ports {
            crate::transport::stop_adb_server(port)?;
        }
        Ok(())
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Abre un link en el teléfono: Android lo manda a la app que lo maneja (un post de Instagram
/// abre Instagram, uno de TikTok abre TikTok, etc.) o al navegador.
#[tauri::command]
pub async fn open_url(serial: String, url: String, state: State<'_, AppState>) -> Result<(), String> {
    let url = url.trim().to_string();
    let lower = url.to_ascii_lowercase();
    if !(lower.starts_with("http://") || lower.starts_with("https://"))
        || url.chars().any(|c| c.is_control() || c.is_whitespace())
    {
        return Err("no es un link válido (tiene que empezar con http:// o https://)".into());
    }
    let manager = state.manager.clone();
    tauri::async_runtime::spawn_blocking(move || {
        // Comillas simples del shell; una comilla dentro del link se escribe como '\''.
        let quoted = format!("'{}'", url.replace('\'', "'\\''"));
        let script = format!("input keyevent 224; am start -a android.intent.action.VIEW -d {quoted}");
        manager.adb().shell(&serial, &script).map(|_| ())
    })
    .await
    .map_err(|e| e.to_string())?
    .map_err(|e| e.to_string())
}

/// Enciende la pantalla y saca la pantalla de bloqueo. Sin PIN ni patrón queda desbloqueado; con
/// PIN o patrón queda en la pantalla para escribirlo. No depende de dónde empiece un
/// deslizamiento: en HyperOS, si el arrastre arranca sobre el candado de abajo, no desbloquea.
#[tauri::command]
pub async fn unlock_screen(serial: String, state: State<'_, AppState>) -> Result<(), String> {
    let manager = state.manager.clone();
    tauri::async_runtime::spawn_blocking(move || {
        manager.adb().shell(&serial, "input keyevent 224; wm dismiss-keyguard").map(|_| ())
    })
    .await
    .map_err(|e| e.to_string())?
    .map_err(|e| e.to_string())
}

/// Abre la pantalla de cuentas de Android en el dispositivo.
#[tauri::command]
pub async fn open_account_settings(serial: String, state: State<'_, AppState>) -> Result<(), String> {
    let manager = state.manager.clone();
    tauri::async_runtime::spawn_blocking(move || {
        manager.adb().shell(&serial, "am start -a android.settings.SYNC_SETTINGS").map(|_| ())
    })
    .await
    .map_err(|e| e.to_string())?
    .map_err(|e| e.to_string())
}

/// Pega texto en el teléfono: lo pone en su portapapeles y simula "pegar" en el campo con
/// foco. A diferencia de escribir tecla por tecla, sirve para links, emojis y acentos.
#[tauri::command]
pub async fn paste_text(serial: String, text: String, state: State<'_, AppState>) -> Result<(), String> {
    let msg = ControlMessage::SetClipboard { sequence: 0, paste: true, text };
    state.manager.send_control(&serial, msg).map_err(|e| e.to_string())
}

/// Carpeta de capturas: Imágenes\Aguará.
fn shots_dir() -> std::path::PathBuf {
    std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(std::path::PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join("Pictures")
        .join("Aguará")
}

/// Abre la carpeta de capturas en el Explorador.
#[tauri::command]
pub fn open_shots_dir() -> Result<(), String> {
    let dir = shots_dir();
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    std::process::Command::new("explorer").arg(&dir).spawn().map(|_| ()).map_err(|e| e.to_string())
}

/// Texto del portapapeles de la PC (para "Pegar" desde el menú, sin Ctrl+V).
#[tauri::command]
pub async fn read_pc_clipboard() -> Result<String, String> {
    tauri::async_runtime::spawn_blocking(|| {
        let mut cmd = std::process::Command::new("powershell");
        cmd.args(["-NoProfile", "-NonInteractive", "-Command",
            "[Console]::OutputEncoding=[Text.Encoding]::UTF8; Get-Clipboard -Raw"]);
        crate::transport::hide_window(&mut cmd);
        let out = cmd.output().map_err(|e| e.to_string())?;
        let text = String::from_utf8_lossy(&out.stdout);
        Ok(text.trim_end_matches(['\r', '\n']).to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Empieza a escuchar en la PC el audio del teléfono.
#[tauri::command]
pub async fn start_audio(serial: String, state: State<'_, AppState>) -> Result<(), String> {
    let manager = state.manager.clone();
    tauri::async_runtime::spawn_blocking(move || manager.start_audio(&serial))
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn stop_audio(serial: String, state: State<'_, AppState>) -> Result<(), String> {
    let manager = state.manager.clone();
    tauri::async_runtime::spawn_blocking(move || manager.stop_audio(&serial))
        .await
        .map_err(|e| e.to_string())
}

/// Serials cuyo audio suena en la PC.
#[tauri::command]
pub fn list_audio(state: State<AppState>) -> Vec<String> {
    state.manager.audio_active()
}

/// Cómo abrir el selector de cuentas de una app: mantener apretado o tocar el primer elemento
/// que cumpla `find`.
struct Opener {
    long_press: bool,
    find: fn(&crate::domain::ui::UiNode) -> bool,
}

/// Pestaña de perfil de la barra inferior (Instagram, Threads, TikTok…).
fn is_profile_tab(n: &crate::domain::ui::UiNode) -> bool {
    const DESCS: &[&str] = &["perfil", "profile", "tu perfil", "your profile", "yo", "me"];
    let d = n.desc.trim().to_lowercase();
    // Facebook: "Perfil, pestaña 6 de 6, …".
    n.id.ends_with(":id/profile_tab")
        || DESCS.contains(&d.as_str())
        || d.starts_with("perfil, pestaña")
        || d.starts_with("profile, tab")
}

/// Menú lateral o botón de cuentas (X y otras con el avatar arriba a la izquierda).
fn is_account_menu(n: &crate::domain::ui::UiNode) -> bool {
    const DESCS: &[&str] = &[
        "mostrar menú de navegación", "show navigation drawer", "abrir menú de navegación",
        "open navigation drawer", "cambiar de cuenta", "switch accounts", "cuentas", "accounts",
    ];
    DESCS.contains(&n.desc.trim().to_lowercase().as_str())
}

const LONG_PRESS_PROFILE: Opener = Opener { long_press: true, find: is_profile_tab };
const TAP_ACCOUNT_MENU: Opener = Opener { long_press: false, find: is_account_menu };

/// App de cada tipo de cuenta de Android y en qué orden probar abrir su selector de cuentas.
/// `openers` vacío = la app no permite cambiar de cuenta (solo se abre).
fn app_for(kind: &str) -> Option<(&'static str, &'static [Opener])> {
    const SOCIAL: &[Opener] = &[LONG_PRESS_PROFILE, TAP_ACCOUNT_MENU];
    Some(match kind {
        "www.instagram.com" | "com.instagram.android" => ("com.instagram.android", &[LONG_PRESS_PROFILE]),
        "www.instagram.barcelona" => ("com.instagram.barcelona", SOCIAL),
        "com.facebook.auth.login" => ("com.facebook.katana", SOCIAL),
        "com.facebook.lite" => ("com.facebook.lite", SOCIAL),
        "com.facebook.messenger" => ("com.facebook.orca", SOCIAL),
        "com.twitter.android.auth.login" => ("com.twitter.android", &[TAP_ACCOUNT_MENU, LONG_PRESS_PROFILE]),
        "com.zhiliaoapp.account" | "com.zhiliaoapp.musically" => ("com.zhiliaoapp.musically", SOCIAL),
        "com.ss.android.ugc.trill" => ("com.ss.android.ugc.trill", SOCIAL),
        "com.linkedin.android" => ("com.linkedin.android", SOCIAL),
        "com.pinterest" => ("com.pinterest", SOCIAL),
        "com.whatsapp" => ("com.whatsapp", &[]),
        "com.whatsapp.w4b" => ("com.whatsapp.w4b", &[]),
        "org.telegram.messenger" => ("org.telegram.messenger", &[]),
        "com.google.android.apps.tachyon" => ("com.google.android.apps.tachyon", &[]),
        _ => return None,
    })
}

/// Nodo que muestra la cuenta `name` (como texto o descripción, con o sin "@").
fn find_account<'a>(nodes: &'a [crate::domain::ui::UiNode], name: &str) -> Option<&'a crate::domain::ui::UiNode> {
    let name = name.trim().trim_start_matches('@').to_lowercase();
    let is = |v: &str| {
        let v = v.trim().trim_start_matches('@').to_lowercase();
        !v.is_empty() && v == name
    };
    // Facebook describe las filas como "Nombre, 3 notificaciones".
    let desc_is = |v: &str| is(v) || v.split(',').next().is_some_and(|first| v.contains(',') && is(first));
    nodes.iter().find(|n| is(&n.text)).or_else(|| nodes.iter().find(|n| desc_is(&n.desc)))
}

/// Entra a una cuenta con un clic: abre la app de esa red social y elige la cuenta en su
/// selector de cuentas, tocando la pantalla como lo haría una persona. Devuelve "switched" si
/// tocó la cuenta, u "opened" si solo pudo abrir la app (sin selector legible o sin varias
/// cuentas, como WhatsApp).
#[tauri::command]
pub async fn switch_account(
    serial: String,
    kind: String,
    name: String,
    state: State<'_, AppState>,
) -> Result<String, String> {
    let manager = state.manager.clone();
    tauri::async_runtime::spawn_blocking(move || {
        use crate::domain::ui::{parse_nodes, UiNode, DUMP_SCRIPT};
        let adb = manager.adb();
        let sh = |s: &str| adb.shell(&serial, s).map_err(|e| e.to_string());
        let dump = || -> Result<Vec<UiNode>, String> { Ok(parse_nodes(&sh(DUMP_SCRIPT)?)) };
        let flag = start_task(&serial);
        let pause = |ms| {
            if !wait_unless_stopped(ms, &flag) {
                return Err("detenido".to_string());
            }
            Ok(())
        };
        let result = (|| -> Result<String, String> {
        let (package, openers) = app_for(&kind).ok_or("esta cuenta no es de una app conocida")?;
        let launch = format!("monkey -p {package} -c android.intent.category.LAUNCHER 1");
        sh("input keyevent 224")?; // encender pantalla si estaba apagada
        sh(&launch)?;
        if openers.is_empty() {
            return Ok("opened".to_string());
        }

        for opener in openers {
            // Llegar a la pantalla principal (la app pudo quedar en un chat, un reel, etc.).
            let mut target = None;
            for _ in 0..5 {
                pause(1200)?;
                let nodes = dump()?;
                if let Some(n) = nodes.iter().find(|n| (opener.find)(n)) {
                    target = Some(n.center);
                    break;
                }
                let in_app = nodes.iter().any(|n| n.package == package);
                if !in_app && nodes.len() > 3 {
                    sh(&launch)?;
                } else if in_app {
                    sh("input keyevent 4")?;
                } else {
                    break; // pantalla ilegible (p. ej. Facebook): probar el siguiente método
                }
            }
            let Some((x, y)) = target else { continue };

            sh(&if opener.long_press {
                format!("input swipe {x} {y} {x} {y} 900")
            } else {
                format!("input tap {x} {y}")
            })?;
            let mut opened_others = false;
            for _ in 0..3 {
                pause(1200)?;
                let nodes = dump()?;
                if let Some(n) = find_account(&nodes, &name) {
                    let (ax, ay) = n.center;
                    sh(&format!("input tap {ax} {ay}"))?;
                    return Ok("switched".to_string());
                }
                // Facebook agrupa el resto de las cuentas bajo "Otras cuentas".
                let others = nodes.iter().find(|n| {
                    let v = n.text.trim().to_lowercase();
                    v == "otras cuentas" || v == "other accounts"
                });
                if let (Some(o), false) = (others, opened_others) {
                    opened_others = true;
                    sh(&format!("input tap {} {}", o.center.0, o.center.1))?;
                    // Android guarda las cuentas de Facebook con su ID numérico, no con el nombre
                    // que muestra la app: dejar la lista abierta para que el usuario elija.
                    if name.trim().chars().all(|c| c.is_ascii_digit()) {
                        return Ok("opened".to_string());
                    }
                    continue;
                }
                // Lista larga: desplazar y volver a buscar.
                sh("input swipe 360 1200 360 700 300")?;
            }
            sh("input keyevent 4")?;
        }
        // La app quedó abierta: el usuario elige la cuenta a mano.
        Ok("opened".to_string())
        })();
        end_task(&serial, &flag);
        result
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Botón "Confirmar" de una solicitud de amistad recibida (nunca "Agregar a amigos", que
/// ENVÍA solicitudes a sugerencias).
fn is_confirm_button(n: &crate::domain::ui::UiNode) -> bool {
    [&n.text, &n.desc].iter().any(|v| {
        let v = v.trim().to_lowercase();
        // Facebook: "Confirmar la solicitud de amistad de <nombre>".
        v == "confirmar"
            || v == "confirm"
            || v.starts_with("confirmar la solicitud")
            || v.starts_with("confirmar solicitud")
            || v.starts_with("confirm request")
            || v.starts_with("confirm friend request")
    })
}

/// Acepta las solicitudes de amistad pendientes en Facebook: lee la pantalla una vez, toca todos
/// los "Confirmar" visibles con ~1 s entre toques, baja y repite. Termina cuando, después de
/// bajar, no queda ninguno, o al llegar a `max` (aceptar cientos de golpe puede hacer que Facebook
/// limite la cuenta). Devuelve cuántas aceptó.
#[tauri::command]
pub async fn accept_friend_requests(serial: String, max: Option<u32>, state: State<'_, AppState>) -> Result<u32, String> {
    let manager = state.manager.clone();
    let max = max.unwrap_or(20).min(50);
    tauri::async_runtime::spawn_blocking(move || {
        use crate::domain::ui::{parse_nodes, DUMP_SCRIPT};
        let adb = manager.adb();
        let sh = |s: &str| adb.shell(&serial, s).map_err(|e| e.to_string());
        let flag = start_task(&serial);
        let result = (|| -> Result<u32, String> {
        // Al pedir detener, se corta en la próxima espera y se devuelve lo aceptado hasta ahí.
        let pause = |ms: u64| wait_unless_stopped(ms, &flag);

        // Encender y sacar el bloqueo: con la pantalla bloqueada Facebook no se ve ni se puede tocar.
        sh("input keyevent 224; wm dismiss-keyguard")?;
        sh("am start -a android.intent.action.VIEW -d fb://friends/requests com.facebook.katana")?;
        if !pause(3500) {
            return Ok(0);
        }
        // La pantalla de Amigos abre en sugerencias: pasar a la pestaña de solicitudes.
        let nodes = parse_nodes(&sh(DUMP_SCRIPT)?);
        if nodes.is_empty() {
            return Err("no pude leer la pantalla de Facebook (¿pantalla bloqueada?)".into());
        }
        if let Some(tab) = nodes.iter().find(|n| {
            let d = n.desc.trim().to_lowercase();
            d == "solicitudes de amistad" || d == "friend requests"
        }) {
            sh(&format!("input tap {} {}", tab.center.0, tab.center.1))?;
            if !pause(1800) {
                return Ok(0);
            }
        }

        let mut accepted = 0u32;
        let mut saw_facebook = nodes.iter().any(|n| n.package == "com.facebook.katana");
        let mut scrolled_empty = false;
        let mut last_batch: Vec<(i32, i32)> = Vec::new();
        let mut last_friends = 0usize;
        // Pausa corta entre toques (0,6–1,1 s): alcanza para que Facebook registre cada uno.
        let gap = || 600 + (std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH).map(|d| d.subsec_millis()).unwrap_or(0) as u64 / 2);
        let now_friends = |n: &crate::domain::ui::UiNode| {
            let v = format!("{} {}", n.text, n.desc).to_lowercase();
            v.contains("ahora son amigos") || v.contains("now friends")
        };
        while accepted < max && !stopped(&flag) {
            // Una sola lectura de pantalla por tanda: leerla tarda 2–3 s.
            let nodes = parse_nodes(&sh(DUMP_SCRIPT)?);
            saw_facebook |= nodes.iter().any(|n| n.package == "com.facebook.katana");
            // Facebook avisa cuando no queda ninguna: terminar ahí, sin recorrer las sugerencias.
            let none_left = nodes.iter().any(|n| {
                let v = format!("{} {}", n.text, n.desc).trim().to_lowercase();
                v.starts_with("no hay solicitudes") || v.starts_with("no new requests") || v.starts_with("no friend requests")
            });
            if none_left {
                break;
            }
            let friends = nodes.iter().filter(|n| now_friends(n)).count();
            // Cada botón aparece dos veces (el botón y su texto): quedarse con el que se toca.
            let mut batch: Vec<(i32, i32)> =
                nodes.iter().filter(|n| n.clickable && is_confirm_button(n)).map(|n| n.center).collect();
            if batch.is_empty() {
                batch = nodes.iter().filter(|n| is_confirm_button(n)).map(|n| n.center).collect();
            }
            if batch.is_empty() {
                // Nada a la vista: bajar una vez por si hay más; si sigue vacío, ya no quedan.
                if scrolled_empty {
                    break;
                }
                scrolled_empty = true;
                sh("input swipe 360 1200 360 500 250")?;
                if !pause(900) {
                    break;
                }
                continue;
            }
            scrolled_empty = false;
            if batch == last_batch && friends <= last_friends {
                // Los mismos botones en el mismo lugar y ningún "Ahora son amigos" nuevo: los
                // toques anteriores no hicieron efecto. No contarlos y no insistir.
                accepted = accepted.saturating_sub(batch.len() as u32);
                if accepted == 0 {
                    return Err("Facebook no respondió a los toques en Confirmar".into());
                }
                break;
            }
            last_friends = friends;
            last_batch = batch.clone();
            // De abajo hacia arriba: confirmar una fila no mueve las que están encima.
            batch.sort_by_key(|c| std::cmp::Reverse(c.1));
            for (x, y) in batch {
                if accepted >= max || stopped(&flag) {
                    break;
                }
                // Si alguien cambió de app en el teléfono, no tocar: el toque caería en otra app.
                let focus = sh("dumpsys window | grep -m1 mCurrentFocus")?;
                if !focus.contains("com.facebook.katana") {
                    return if accepted > 0 {
                        Ok(accepted)
                    } else {
                        Err("se cambió de app en el teléfono; frené para no tocar otra cosa".into())
                    };
                }
                sh(&format!("input tap {x} {y}"))?;
                accepted += 1;
                if !pause(gap()) {
                    break;
                }
            }
        }
        // Con 0 aceptadas y sin haber visto Facebook, decir por qué. Si Facebook abrió, 0 = no
        // había pendientes (la app lo muestra así).
        if accepted == 0 && !saw_facebook && !stopped(&flag) {
            return Err("Facebook no se abrió en la pantalla de solicitudes (¿está instalado y con la sesión iniciada?)".into());
        }
        Ok(accepted)
        })();
        end_task(&serial, &flag);
        result
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Reinicia el dispositivo (la UI pide confirmación antes).
#[tauri::command]
pub async fn reboot_device(serial: String, state: State<'_, AppState>) -> Result<(), String> {
    let manager = state.manager.clone();
    tauri::async_runtime::spawn_blocking(move || manager.adb().reboot(&serial))
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| e.to_string())
}

/// Construye el SessionManager y conecta el sink de frames a un evento Tauri.
pub fn build_state(app: &tauri::AppHandle) -> AppState {
    use crate::app::SessionManager;
    use crate::media::Frame;
    use crate::transport::AdbClient;

    let handle = app.clone();
    let sink: crate::app::FrameSink = std::sync::Arc::new(move |serial: &str, frame: Frame| {
        let _ = handle.emit(
            "video-frame",
            VideoFramePayload {
                serial: serial.to_string(),
                width: frame.width,
                height: frame.height,
                jpeg: base64_encode(&frame.jpeg),
            },
        );
    });

    let end_handle = app.clone();
    let end_sink: crate::app::EndSink = std::sync::Arc::new(move |serial: &str| {
        let _ = end_handle.emit("session-ended", serial.to_string());
    });

    // Jar del server junto al ejecutable (vendor/ copiado en el bundle) o en src-tauri/vendor.
    let jar_path = app
        .path()
        .resolve("vendor/scrcpy-server.jar", tauri::path::BaseDirectory::Resource)
        .unwrap_or_else(|_| std::path::PathBuf::from("vendor/scrcpy-server.jar"));

    let manager = SessionManager::new(AdbClient::from_path(), jar_path, sink, end_sink);
    AppState { manager: std::sync::Arc::new(manager) }
}

#[cfg(test)]
mod tests {
    use super::base64_encode;

    #[test]
    fn base64_rfc4648_vectors() {
        assert_eq!(base64_encode(b""), "");
        assert_eq!(base64_encode(b"f"), "Zg==");
        assert_eq!(base64_encode(b"fo"), "Zm8=");
        assert_eq!(base64_encode(b"foo"), "Zm9v");
        assert_eq!(base64_encode(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn stop_tasks_cuts_a_running_wait() {
        use super::{end_task, start_task, stop_tasks, wait_unless_stopped};
        let flag = start_task("tel-prueba");
        let waiter = {
            let flag = flag.clone();
            std::thread::spawn(move || {
                let t0 = std::time::Instant::now();
                (wait_unless_stopped(10_000, &flag), t0.elapsed())
            })
        };
        std::thread::sleep(std::time::Duration::from_millis(300));
        stop_tasks(vec!["tel-prueba".into()]);
        let (finished, took) = waiter.join().unwrap();
        assert!(!finished, "la espera tenía que cortarse");
        assert!(took < std::time::Duration::from_secs(1), "tardó {took:?} en detenerse");
        end_task("tel-prueba", &flag);
    }
}
