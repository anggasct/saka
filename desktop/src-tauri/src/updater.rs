use tauri::utils::config::Config;
use tauri::{AppHandle, Manager as _, Runtime};
use tauri_plugin_updater::{Update, UpdaterExt};

use crate::shell::{self, Ui};

const FIRST_CHECK_AFTER: std::time::Duration = std::time::Duration::from_secs(20);
const CHECK_EVERY: std::time::Duration = std::time::Duration::from_secs(6 * 3600);

pub fn configured(config: &Config) -> bool {
    config
        .plugins
        .0
        .get("updater")
        .and_then(|section| section.get("pubkey"))
        .and_then(|key| key.as_str())
        .is_some_and(|key| !key.trim().is_empty())
}

pub fn spawn_checks<R: Runtime>(app: &AppHandle<R>) {
    if !configured(app.config()) {
        return;
    }
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(FIRST_CHECK_AFTER).await;
        loop {
            check(&app).await;
            tokio::time::sleep(CHECK_EVERY).await;
        }
    });
}

async fn check<R: Runtime>(app: &AppHandle<R>) {
    let found = match app.updater() {
        Ok(updater) => updater.check().await,
        Err(e) => {
            tracing::warn!(error = %e, "updater unavailable");
            return;
        }
    };
    match found {
        Ok(Some(update)) => shell::offer_update(app, update),
        Ok(None) => {}
        Err(e) => tracing::debug!(error = %e, "update check failed"),
    }
}

pub async fn install<R: Runtime>(app: &AppHandle<R>, update: Update) {
    match update.download_and_install(|_, _| {}, || {}).await {
        Ok(()) => app.restart(),
        Err(e) => tracing::warn!(error = %e, "update not installed"),
    }
}

pub fn take_pending<R: Runtime>(app: &AppHandle<R>) -> Option<Update> {
    app.state::<Ui>().take_update()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(plugins: serde_json::Value) -> Config {
        let mut base: serde_json::Value =
            serde_json::from_str(include_str!("../tauri.conf.json")).unwrap();
        base["plugins"] = plugins;
        serde_json::from_value(base).unwrap()
    }

    #[test]
    fn the_updater_is_off_unless_a_public_key_is_set() {
        assert!(!configured(&config(serde_json::json!({}))));
        assert!(!configured(&config(
            serde_json::json!({ "updater": { "pubkey": "" } })
        )));
        assert!(!configured(&config(
            serde_json::json!({ "updater": { "pubkey": "   ", "endpoints": ["https://u.test"] } })
        )));
        assert!(!configured(&config(
            serde_json::json!({ "updater": { "endpoints": ["https://u.test"] } })
        )));
        assert!(configured(&config(serde_json::json!({
            "updater": { "pubkey": "dW50cnVzdGVk", "endpoints": ["https://u.test"] }
        }))));
    }

    #[test]
    fn the_shipped_config_leaves_the_updater_off() {
        let shipped: Config = serde_json::from_str(include_str!("../tauri.conf.json")).unwrap();
        assert!(!configured(&shipped));
    }
}
