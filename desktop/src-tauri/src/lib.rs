mod address;
mod commands;
mod csp;
mod error;
mod manager;
mod pairing;
mod secrets;
mod store;

use std::sync::Arc;

use tauri::Manager as _;
use tauri::utils::config::Csp;

use crate::manager::Manager;
use crate::secrets::Keychain;
use crate::store::Store;

fn data_dir(identifier: &str) -> std::path::PathBuf {
    dirs::data_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join(identifier)
}

pub fn run() {
    tracing_subscriber::fmt().with_target(false).init();
    let mut context = tauri::generate_context!();
    let store = Store::new(data_dir(&context.config().identifier).join("connections.json"));
    let http = match pairing::client() {
        Ok(http) => http,
        Err(e) => {
            tracing::error!(code = e.code(), "desktop shell cannot start");
            return;
        }
    };
    let manager = Manager::new(store, Arc::new(Keychain), http);
    let active = manager.active_url();
    context.config_mut().app.security.csp = Some(Csp::Policy(csp::policy(active.as_deref())));

    let built = tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.unminimize();
                let _ = window.show();
                let _ = window.set_focus();
            }
        }))
        .manage(manager)
        .invoke_handler(tauri::generate_handler![
            commands::connections_list,
            commands::connection_add,
            commands::connection_remove,
            commands::connection_select,
        ])
        .build(context);
    match built {
        Ok(app) => app.run(|_, _| {}),
        Err(e) => tracing::error!(error = %e, "desktop shell failed to start"),
    }
}
