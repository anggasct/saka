use tauri::State;

use crate::error::ShellError;
use crate::inbox::{self, Notice};
use crate::manager::{Listing, Manager, View};
use crate::shell;

#[tauri::command(rename_all = "snake_case")]
pub fn connections_list(manager: State<'_, Manager>) -> Result<Listing, ShellError> {
    manager.list()
}

#[tauri::command(rename_all = "snake_case")]
pub async fn connection_add(
    manager: State<'_, Manager>,
    name: String,
    url: String,
    pairing_code: String,
) -> Result<View, ShellError> {
    manager.add(&name, &url, &pairing_code).await
}

#[tauri::command(rename_all = "snake_case")]
pub fn connection_remove(manager: State<'_, Manager>, id: String) -> Result<(), ShellError> {
    manager.remove(&id)
}

#[tauri::command(rename_all = "snake_case")]
pub fn connection_select(
    app: tauri::AppHandle,
    manager: State<'_, Manager>,
    id: String,
) -> Result<(), ShellError> {
    manager.select(&id)?;
    app.restart()
}

#[tauri::command(rename_all = "snake_case")]
pub async fn local_start(
    app: tauri::AppHandle,
    manager: State<'_, Manager>,
) -> Result<(), ShellError> {
    let result = manager.local_start().await;
    shell::refresh_tray(&app);
    result
}

#[tauri::command(rename_all = "snake_case")]
pub async fn local_stop(
    app: tauri::AppHandle,
    manager: State<'_, Manager>,
) -> Result<(), ShellError> {
    manager.local_stop().await;
    shell::refresh_tray(&app);
    Ok(())
}

#[tauri::command(rename_all = "snake_case")]
pub fn notify(app: tauri::AppHandle, title: String, body: String, route: String) {
    shell::notify(
        &app,
        &Notice {
            title,
            body,
            route: inbox::safe_route(&route),
        },
    );
}
