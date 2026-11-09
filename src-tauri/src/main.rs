// En release no abrir consola en Windows.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod commands;
mod domain;
mod media;
mod transport;

use commands::{
    build_state, device_details, find_template, inject_key, inject_scroll, inject_text, inject_touch, install_apk,
    launch_app, list_active, list_audio, list_devices, open_account_settings, paste_text, push_file,
    read_pc_clipboard, reboot_device, screenshot, start_audio, start_mirror, stop_audio, stop_mirror,
    switch_account, accept_friend_requests, open_shots_dir, open_url, panic_wipe, foreign_adb, free_adb, stop_tasks,
    unlock_screen,
};
use tauri::Manager;

fn main() {
    tauri::Builder::default()
        .setup(|app| {
            // ffmpeg viene en el instalador; adb se descarga la primera vez si falta. En segundo
            // plano para que la ventana abra enseguida.
            std::thread::spawn(|| {
                if !ffmpeg_sidecar::paths::ffmpeg_path().exists() {
                    if let Err(e) = ffmpeg_sidecar::download::auto_download() {
                        eprintln!("aviso: no se pudo preparar ffmpeg automáticamente: {e}");
                    }
                }
                if let Err(e) = transport::ensure_adb() {
                    eprintln!("aviso: {e}");
                }
            });
            let state = build_state(&app.handle());
            app.manage(state);
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            list_devices,
            list_active,
            start_mirror,
            stop_mirror,
            inject_touch,
            inject_key,
            inject_text,
            inject_scroll,
            find_template,
            install_apk,
            push_file,
            screenshot,
            device_details,
            launch_app,
            open_account_settings,
            unlock_screen,
            reboot_device,
            paste_text,
            read_pc_clipboard,
            start_audio,
            stop_audio,
            list_audio,
            switch_account,
            accept_friend_requests,
            open_shots_dir,
            open_url,
            panic_wipe,
            foreign_adb,
            free_adb,
            stop_tasks
        ])
        .run(tauri::generate_context!())
        .expect("error al arrancar Aguará");
}
