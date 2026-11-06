use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::time::{Duration, Instant};

use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::TrayIconBuilder;
use tauri::{AppHandle, Emitter, Manager as _, Runtime, WebviewWindow};
use tauri_plugin_notification::{NotificationExt, PermissionState};
use tauri_plugin_updater::Update;

use crate::inbox::{self, Notice, Watcher};
use crate::labels::Labels;
use crate::manager::Manager;
use crate::sidecar::Phase;
use crate::tray::{self, Mode};
use crate::updater;

pub const NAVIGATE_EVENT: &str = "saka://navigate";
const TRAY_ID: &str = "main";
const POLL_EVERY: Duration = Duration::from_secs(30);
const PENDING_FOR: Duration = Duration::from_secs(300);
const NO_COUNT: i64 = -1;

pub struct Ui {
    pub labels: Labels,
    open: AtomicI64,
    pub tray: AtomicBool,
    pending: Mutex<Option<(String, Instant)>>,
    update: Mutex<Option<Update>>,
}

impl Ui {
    pub fn new(labels: Labels) -> Ui {
        Ui {
            labels,
            open: AtomicI64::new(NO_COUNT),
            tray: AtomicBool::new(false),
            pending: Mutex::new(None),
            update: Mutex::new(None),
        }
    }

    fn offered_version(&self) -> Option<String> {
        self.update.lock().ok()?.as_ref().map(|u| u.version.clone())
    }

    pub fn take_update(&self) -> Option<Update> {
        self.update.lock().ok()?.take()
    }

    fn open(&self) -> Option<i64> {
        let count = self.open.load(Ordering::Relaxed);
        (count != NO_COUNT).then_some(count)
    }

    fn remember(&self, route: String) {
        if let Ok(mut slot) = self.pending.lock() {
            *slot = Some((route, Instant::now()));
        }
    }

    pub fn take_pending(&self) -> Option<String> {
        let (route, at) = self.pending.lock().ok()?.take()?;
        (at.elapsed() < PENDING_FOR).then_some(route)
    }
}

fn main_window<R: Runtime>(app: &AppHandle<R>) -> Option<WebviewWindow<R>> {
    app.get_webview_window("main")
}

pub fn show_main<R: Runtime>(app: &AppHandle<R>) {
    if let Some(window) = main_window(app) {
        let _ = window.unminimize();
        let _ = window.show();
        let _ = window.set_focus();
    }
}

pub fn navigate<R: Runtime>(app: &AppHandle<R>, route: &str) {
    show_main(app);
    let _ = app.emit_to("main", NAVIGATE_EVENT, inbox::safe_route(route));
}

fn window_is_focused<R: Runtime>(app: &AppHandle<R>) -> bool {
    main_window(app).is_some_and(|w| w.is_focused().unwrap_or(false))
}

pub fn notify<R: Runtime>(app: &AppHandle<R>, notice: &Notice) {
    let notifications = app.notification();
    let granted = matches!(
        notifications.permission_state(),
        Ok(PermissionState::Granted)
    ) || matches!(
        notifications.request_permission(),
        Ok(PermissionState::Granted)
    );
    if !granted {
        tracing::debug!("notification permission not granted");
        return;
    }
    if let Err(e) = notifications
        .builder()
        .title(&notice.title)
        .body(&notice.body)
        .show()
    {
        tracing::warn!(error = %e, "notification not shown");
        return;
    }
    app.state::<Ui>().remember(inbox::safe_route(&notice.route));
}

fn mode<R: Runtime>(app: &AppHandle<R>) -> Mode {
    let manager = app.state::<Manager>();
    if manager.local_is_active() {
        Mode::Local(manager.sidecar().phase())
    } else {
        Mode::Remote
    }
}

fn build_menu<R: Runtime>(app: &AppHandle<R>) -> tauri::Result<Menu<R>> {
    let ui = app.state::<Ui>();
    let menu = Menu::new(app)?;
    let offered = ui.offered_version();
    for entry in tray::menu(mode(app), ui.open(), offered.as_deref(), ui.labels) {
        if entry.id == tray::QUIT {
            menu.append(&PredefinedMenuItem::separator(app)?)?;
        }
        menu.append(&MenuItem::with_id(
            app,
            entry.id,
            entry.label,
            entry.enabled,
            None::<&str>,
        )?)?;
    }
    Ok(menu)
}

pub fn refresh_tray<R: Runtime>(app: &AppHandle<R>) {
    let Some(tray_icon) = app.tray_by_id(TRAY_ID) else {
        return;
    };
    let ui = app.state::<Ui>();
    if let Ok(menu) = build_menu(app) {
        let _ = tray_icon.set_menu(Some(menu));
    }
    let _ = tray_icon.set_tooltip(Some(tray::tooltip(ui.open(), ui.labels)));
}

pub fn build_tray<R: Runtime>(app: &AppHandle<R>) -> tauri::Result<()> {
    let mut builder = TrayIconBuilder::with_id(TRAY_ID)
        .menu(&build_menu(app)?)
        .tooltip("Saka")
        .on_menu_event(|app, event| on_menu(app, event.id().as_ref()));
    if let Some(icon) = app.default_window_icon() {
        builder = builder.icon(icon.clone());
    }
    builder.build(app)?;
    app.state::<Ui>().tray.store(true, Ordering::Relaxed);
    Ok(())
}

fn on_menu<R: Runtime>(app: &AppHandle<R>, id: &str) {
    match id {
        tray::OPEN => show_main(app),
        tray::INBOX => navigate(app, "/"),
        tray::START => {
            let app = app.clone();
            tauri::async_runtime::spawn(async move {
                let result = app.state::<Manager>().local_start().await;
                if let Err(e) = result {
                    tracing::warn!(code = e.code(), "local saka did not start from the tray");
                }
                refresh_tray(&app);
            });
        }
        tray::STOP => {
            let app = app.clone();
            tauri::async_runtime::spawn(async move {
                app.state::<Manager>().local_stop().await;
                refresh_tray(&app);
            });
        }
        tray::UPDATE => {
            let app = app.clone();
            tauri::async_runtime::spawn(async move {
                if let Some(update) = updater::take_pending(&app) {
                    app.state::<Manager>().local_stop().await;
                    updater::install(&app, update).await;
                }
            });
        }
        tray::QUIT => {
            let app = app.clone();
            tauri::async_runtime::spawn(async move {
                app.state::<Manager>().local_stop().await;
                app.exit(0);
            });
        }
        _ => {}
    }
}

pub fn offer_update<R: Runtime>(app: &AppHandle<R>, update: Update) {
    let version = update.version.clone();
    let ui = app.state::<Ui>();
    if let Ok(mut slot) = ui.update.lock() {
        *slot = Some(update);
    }
    refresh_tray(app);
    notify(
        app,
        &Notice {
            title: "Saka".to_string(),
            body: ui.labels.update_available(&version),
            route: "/".to_string(),
        },
    );
}

pub fn on_focus<R: Runtime>(app: &AppHandle<R>) {
    if let Some(route) = app.state::<Ui>().take_pending() {
        let _ = app.emit_to("main", NAVIGATE_EVENT, route);
    }
}

pub fn spawn_background<R: Runtime>(app: &AppHandle<R>) {
    let watcher_app = app.clone();
    tauri::async_runtime::spawn(async move {
        let mut phases = watcher_app.state::<Manager>().sidecar().watch();
        while phases.changed().await.is_ok() {
            refresh_tray(&watcher_app);
        }
    });
    let poll_app = app.clone();
    tauri::async_runtime::spawn(async move {
        let mut watcher = Watcher::default();
        loop {
            poll_once(&poll_app, &mut watcher).await;
            tokio::time::sleep(POLL_EVERY).await;
        }
    });
}

async fn poll_once<R: Runtime>(app: &AppHandle<R>, watcher: &mut Watcher) {
    let manager = app.state::<Manager>();
    let ui = app.state::<Ui>();
    let Some((base, token)) = manager.active_target() else {
        watcher.reset();
        return;
    };
    if manager.local_is_active() && manager.sidecar().phase() != Phase::Running {
        watcher.reset();
        return;
    }
    match watcher
        .tick(manager.http(), &base, token.as_deref(), ui.labels)
        .await
    {
        Ok(tick) => {
            ui.open.store(tick.open, Ordering::Relaxed);
            refresh_tray(app);
            if let Some(notice) = tick.notice
                && !window_is_focused(app)
            {
                notify(app, &notice);
            }
        }
        Err(e) => tracing::debug!(code = e.code(), "inbox poll skipped"),
    }
}
