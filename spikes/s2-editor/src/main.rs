//! Spike S2 server: serves a vault working copy to the editor prototype.
//! Text goes out without its BOM and with the file's line-ending style
//! named, and must come back byte-identical.
//!
//! Usage: s2-editor <vault> <web-dist> [port]

use std::fs;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use axum::extract::{DefaultBodyLimit, Query, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{Value, json};
use tower_http::services::ServeDir;

const BOM: &[u8] = b"\xEF\xBB\xBF";

struct App {
    vault: PathBuf,
    out: PathBuf,
}

#[derive(Deserialize)]
struct FileQuery {
    path: String,
    force: Option<bool>,
}

#[derive(Deserialize)]
struct Save {
    text: String,
    bom: bool,
}

#[derive(Deserialize)]
struct Report {
    name: String,
    data: Value,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let [_, vault, web, rest @ ..] = args.as_slice() else {
        anyhow::bail!("usage: s2-editor <vault> <web-dist> [port]");
    };
    let port: u16 = rest.first().map(|p| p.parse()).transpose()?.unwrap_or(7420);
    let vault = fs::canonicalize(vault)?;
    let out = vault.parent().unwrap_or(&vault).join("out");
    fs::create_dir_all(&out)?;
    let app = Arc::new(App { vault, out });
    let router = Router::new()
        .route("/api/files", get(files))
        .route("/api/file", get(read_file).put(write_file))
        .route("/api/report", post(report))
        .fallback_service(ServeDir::new(web))
        .layer(DefaultBodyLimit::max(64 << 20))
        .with_state(app);
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", port)).await?;
    println!("s2-editor on http://127.0.0.1:{port}");
    axum::serve(listener, router).await?;
    Ok(())
}

async fn files(State(app): State<Arc<App>>) -> Json<Value> {
    let mut out = Vec::new();
    walk(&app.vault, "", &mut out);
    Json(json!({ "files": out }))
}

/// Every file Obsidian would see: hidden entries and symlinks are skipped
/// (real vaults can hold a symlink pointing at their own parent folder).
fn walk(dir: &Path, rel: &str, out: &mut Vec<String>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    let mut entries: Vec<_> = entries.flatten().collect();
    entries.sort_by_key(|e| e.file_name());
    for e in entries {
        let name = e.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') {
            continue;
        }
        let path = if rel.is_empty() {
            name
        } else {
            format!("{rel}/{name}")
        };
        match e.file_type() {
            Ok(t) if t.is_symlink() => {}
            Ok(t) if t.is_dir() => walk(&e.path(), &path, out),
            Ok(_) => out.push(path),
            Err(_) => {}
        }
    }
}

fn resolve(app: &App, rel: &str) -> Result<PathBuf, StatusCode> {
    let p = Path::new(rel);
    if rel.is_empty() || p.components().any(|c| !matches!(c, Component::Normal(_))) {
        return Err(StatusCode::BAD_REQUEST);
    }
    Ok(app.vault.join(p))
}

async fn read_file(
    State(app): State<Arc<App>>,
    Query(q): Query<FileQuery>,
) -> Result<Json<Value>, StatusCode> {
    let path = resolve(&app, &q.path)?;
    let bytes = fs::read(&path).map_err(|_| StatusCode::NOT_FOUND)?;
    let bom = bytes.starts_with(BOM);
    let body = if bom { &bytes[BOM.len()..] } else { &bytes[..] };
    let Ok(text) = std::str::from_utf8(body) else {
        return Ok(Json(
            json!({ "path": q.path, "binary": true, "bytes": bytes.len() }),
        ));
    };
    let crlf = text.matches("\r\n").count();
    let lf = text.matches('\n').count() - crlf;
    let cr = text.matches('\r').count() - crlf;
    let eol = match (crlf, lf, cr) {
        (0, 0, 0) => "none",
        (0, _, 0) => "lf",
        (_, 0, 0) => "crlf",
        _ => "mixed",
    };
    Ok(Json(
        json!({ "path": q.path, "binary": false, "text": text, "bom": bom, "eol": eol, "bytes": bytes.len() }),
    ))
}

/// Writes only when the bytes differ, unless `force` (the round-trip test
/// forces every write so the save path itself is exercised).
async fn write_file(
    State(app): State<Arc<App>>,
    Query(q): Query<FileQuery>,
    Json(save): Json<Save>,
) -> Result<Json<Value>, StatusCode> {
    let path = resolve(&app, &q.path)?;
    let mut bytes = Vec::with_capacity(save.text.len() + 3);
    if save.bom {
        bytes.extend_from_slice(BOM);
    }
    bytes.extend_from_slice(save.text.as_bytes());
    let same = fs::read(&path).map(|old| old == bytes).unwrap_or(false);
    if same && !q.force.unwrap_or(false) {
        return Ok(Json(json!({ "written": false, "same": true })));
    }
    fs::write(&path, &bytes).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(json!({ "written": true, "same": same })))
}

async fn report(
    State(app): State<Arc<App>>,
    Json(r): Json<Report>,
) -> Result<Json<Value>, StatusCode> {
    if r.name.contains(['/', '\\']) || r.name.starts_with('.') {
        return Err(StatusCode::BAD_REQUEST);
    }
    let path = app.out.join(format!("{}.json", r.name));
    let text = serde_json::to_string_pretty(&r.data).map_err(|_| StatusCode::BAD_REQUEST)?;
    fs::write(&path, text).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(json!({ "saved": path })))
}
