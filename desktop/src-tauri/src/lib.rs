mod address;
mod commands;
mod csp;
mod error;
mod inbox;
mod labels;
mod manager;
mod pairing;
mod secrets;
mod shell;
mod sidecar;
mod store;
mod tray;

use std::sync::Arc;
use std::sync::atomic::Ordering;

use tauri::utils::config::Csp;
use tauri::{Manager as _, RunEvent, WindowEvent};

use crate::labels::Labels;
use crate::manager::Manager;
use crate::secrets::Keychain;
use crate::shell::Ui;
use crate::sidecar::{Sidecar, Timing};
use crate::store::Store;

fn data_dir(identifier: &str) -> std::path::PathBuf {
    dirs::data_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join(identifier)
}

pub fn run() {
    tracing_subscriber::fmt().with_target(false).init();
    let mut context = tauri::generate_context!();
    let root = data_dir(&context.config().identifier);
    let http = match pairing::client() {
        Ok(http) => http,
        Err(e) => {
            tracing::error!(code = e.code(), "desktop shell cannot start");
            return;
        }
    };
    let sidecar = Sidecar::new(
        sidecar::bundled_program(),
        root.join("local"),
        http.clone(),
        Timing::default(),
    );
    let manager = Manager::new(
        Store::new(root.join("connections.json")),
        Arc::new(Keychain),
        http,
        sidecar,
    );
    if let Err(e) = manager.prepare_local_port() {
        tracing::warn!(code = e.code(), "no port reserved for the local saka");
    }
    let active = manager.active_origin();
    context.config_mut().app.security.csp = Some(Csp::Policy(csp::policy(active.as_deref())));

    let built = tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            shell::show_main(app);
        }))
        .plugin(tauri_plugin_notification::init())
        .manage(manager)
        .manage(Ui::new(Labels::detect()))
        .setup(|app| {
            let handle = app.handle();
            if let Err(e) = shell::build_tray(handle) {
                tracing::warn!(error = %e, "no tray icon; closing the window quits the app");
            }
            shell::spawn_background(handle);
            Ok(())
        })
        .on_window_event(|window, event| match event {
            WindowEvent::CloseRequested { api, .. }
                if window
                    .app_handle()
                    .state::<Ui>()
                    .tray
                    .load(Ordering::Relaxed) =>
            {
                api.prevent_close();
                let _ = window.hide();
            }
            WindowEvent::Focused(true) => shell::on_focus(window.app_handle()),
            _ => {}
        })
        .invoke_handler(tauri::generate_handler![
            commands::connections_list,
            commands::connection_add,
            commands::connection_remove,
            commands::connection_select,
            commands::local_start,
            commands::local_stop,
            commands::notify,
        ])
        .build(context);
    match built {
        Ok(app) => app.run(|handle, event| {
            if let RunEvent::Exit = event {
                tauri::async_runtime::block_on(handle.state::<Manager>().local_stop());
            }
        }),
        Err(e) => tracing::error!(error = %e, "desktop shell failed to start"),
    }
}
