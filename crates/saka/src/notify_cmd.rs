use std::process::ExitCode;
use std::sync::Arc;

use saka_ports::{Clock, Notification, Notifier};
use serde_json::json;
use uuid::Uuid;

use crate::cli;
use crate::config::Config;
use crate::serve::{EXIT_REFUSED, SystemClock, VERSION};

pub fn test(cfg: &Config, channel: &str, json: bool) -> ExitCode {
    if !cfg.notify.names().any(|name| name == channel) {
        cli::refuse(
            "notify_channel_unconfigured",
            &json!({ "channel": channel }),
        );
        return ExitCode::from(EXIT_REFUSED);
    }
    let clock: Arc<dyn Clock> = Arc::new(SystemClock);
    let notifier = match cfg.chat_notifier(&format!("saka/{VERSION}"), &clock) {
        Ok(n) => n,
        Err(e) => {
            cli::refuse(e.code, &json!({}));
            return ExitCode::from(EXIT_REFUSED);
        }
    };
    let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    else {
        return ExitCode::FAILURE;
    };
    let message = Notification {
        channel: channel.to_string(),
        code: "notify.test".into(),
        params: json!({}),
        dedupe_key: Uuid::now_v7().to_string(),
    };
    match runtime.block_on(notifier.send(&message)) {
        Ok(()) => {
            if json {
                cli::out(&json!({ "channel": channel, "sent": true }).to_string());
            } else {
                cli::out(&format!("Sent a test message to {channel}."));
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            cli::refuse(&e.code, &e.params);
            ExitCode::FAILURE
        }
    }
}
