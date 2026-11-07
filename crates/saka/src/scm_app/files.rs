use std::fs::{DirBuilder, File, OpenOptions};
use std::io::{ErrorKind, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use saka_ports::BestEffort;
use toml_edit::{ArrayOfTables, DocumentMut, Item, Table, value};

pub(crate) const PRIVATE_KEY_FILE: &str = "private-key.pem";
pub(crate) const WEBHOOK_SECRET_FILE: &str = "webhook-secret";
const PRIVATE_DIR_MODE: u32 = 0o700;
const PRIVATE_FILE_MODE: u32 = 0o600;

#[derive(Debug, Clone)]
pub(crate) struct ConfigBlock<'a> {
    pub id: &'a str,
    pub web_url: Option<&'a str>,
    pub api_url: Option<&'a str>,
    pub app_id: i64,
    pub private_key_file: &'a str,
    pub client_id: &'a str,
    pub webhook_secret_file: Option<&'a str>,
    pub hooks_listen: Option<&'a str>,
}

#[derive(Debug)]
pub(crate) enum WriteError {
    Exists(PathBuf),
    Io(PathBuf, std::io::Error),
}

pub(crate) fn block_text(block: &ConfigBlock<'_>) -> String {
    let mut doc = DocumentMut::new();
    if let Some(listen) = block.hooks_listen {
        let mut hooks = Table::new();
        hooks.insert("listen", value(listen));
        doc.insert("hooks", Item::Table(hooks));
    }
    let mut scm = Table::new();
    scm.insert("id", value(block.id));
    scm.insert("provider", value("github"));
    if let Some(web_url) = block.web_url {
        scm.insert("web_url", value(web_url));
    }
    if let Some(api_url) = block.api_url {
        scm.insert("api_url", value(api_url));
    }
    scm.insert("app_id", value(block.app_id));
    scm.insert("private_key_file", value(block.private_key_file));
    scm.insert("client_id", value(block.client_id));
    if let Some(file) = block.webhook_secret_file {
        scm.insert("webhook_secret_file", value(file));
    }
    if block.hooks_listen.is_some() {
        scm.decor_mut().set_prefix("\n");
    }
    let mut entries = ArrayOfTables::new();
    entries.push(scm);
    doc.insert("scm", Item::ArrayOfTables(entries));
    doc.to_string()
}

pub(crate) fn appended(original: &str, block: &str) -> String {
    let mut text = original.to_string();
    if !text.is_empty() {
        if !text.ends_with('\n') {
            text.push('\n');
        }
        text.push('\n');
    }
    text.push_str(block);
    text
}

pub(crate) fn private_dir(dir: &Path) -> std::io::Result<()> {
    DirBuilder::new()
        .recursive(true)
        .mode(PRIVATE_DIR_MODE)
        .create(dir)
}

fn sync_dir(dir: &Path) -> std::io::Result<()> {
    File::open(dir)?.sync_all()
}

fn temp_path(dir: &Path, name: &str) -> std::io::Result<PathBuf> {
    let mut suffix = [0u8; 4];
    getrandom::fill(&mut suffix).map_err(std::io::Error::other)?;
    let hex: String = suffix.iter().map(|b| format!("{b:02x}")).collect();
    Ok(dir.join(format!(".{name}.tmp-{hex}")))
}

fn write_temp(temp: &Path, bytes: &[u8], mode: u32) -> std::io::Result<()> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(mode)
        .open(temp)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    file.set_permissions(std::fs::Permissions::from_mode(mode))
}

pub(crate) fn write_new_private_file(
    dir: &Path,
    name: &str,
    bytes: &[u8],
) -> Result<PathBuf, WriteError> {
    let target = dir.join(name);
    let io = |e| WriteError::Io(target.clone(), e);
    private_dir(dir).map_err(io)?;
    let temp = temp_path(dir, name).map_err(io)?;
    if let Err(e) = write_temp(&temp, bytes, PRIVATE_FILE_MODE) {
        std::fs::remove_file(&temp).best_effort("remove the temp file of a failed write");
        return Err(io(e));
    }
    // A hard link fails when the name exists, so a file that appeared since the preflight is
    // never replaced, unlike a rename.
    let linked = std::fs::hard_link(&temp, &target);
    std::fs::remove_file(&temp).best_effort("remove the temp file");
    match linked {
        Ok(()) => {}
        Err(e) if e.kind() == ErrorKind::AlreadyExists => return Err(WriteError::Exists(target)),
        Err(e) => return Err(io(e)),
    }
    sync_dir(dir).map_err(io)?;
    Ok(target)
}

pub(crate) fn read_optional(path: &Path) -> std::io::Result<Option<Vec<u8>>> {
    match std::fs::read(path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

pub(crate) fn replace_config(path: &Path, text: &str) -> std::io::Result<()> {
    let target = match std::fs::canonicalize(path) {
        Ok(real) => real,
        Err(e) if e.kind() == ErrorKind::NotFound => path.to_path_buf(),
        Err(e) => return Err(e),
    };
    let dir = target
        .parent()
        .ok_or_else(|| std::io::Error::new(ErrorKind::InvalidInput, "no parent folder"))?;
    let mode = match std::fs::metadata(&target) {
        Ok(meta) => meta.permissions().mode() & 0o7777,
        Err(e) if e.kind() == ErrorKind::NotFound => {
            private_dir(dir)?;
            PRIVATE_FILE_MODE
        }
        Err(e) => return Err(e),
    };
    let name = target
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("config.toml");
    let temp = temp_path(dir, name)?;
    if let Err(e) = write_temp(&temp, text.as_bytes(), mode) {
        std::fs::remove_file(&temp).best_effort("remove the temp file of a failed write");
        return Err(e);
    }
    if let Err(e) = std::fs::rename(&temp, &target) {
        std::fs::remove_file(&temp).best_effort("remove the temp file of a failed rename");
        return Err(e);
    }
    sync_dir(dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn block(hooks: Option<&'static str>) -> ConfigBlock<'static> {
        ConfigBlock {
            id: "github",
            web_url: None,
            api_url: None,
            app_id: 123_456,
            private_key_file: "/home/u/.config/saka/scm/github/private-key.pem",
            client_id: "Iv23liAbCdEfGh123456",
            webhook_secret_file: Some("/home/u/.config/saka/scm/github/webhook-secret"),
            hooks_listen: hooks,
        }
    }

    #[test]
    fn the_block_has_hooks_then_the_integration_separated_by_a_blank_line() {
        assert_eq!(
            block_text(&block(Some("127.0.0.1:7421"))),
            concat!(
                "[hooks]\n",
                "listen = \"127.0.0.1:7421\"\n",
                "\n",
                "[[scm]]\n",
                "id = \"github\"\n",
                "provider = \"github\"\n",
                "app_id = 123456\n",
                "private_key_file = \"/home/u/.config/saka/scm/github/private-key.pem\"\n",
                "client_id = \"Iv23liAbCdEfGh123456\"\n",
                "webhook_secret_file = \"/home/u/.config/saka/scm/github/webhook-secret\"\n",
            )
        );
    }

    #[test]
    fn urls_follow_the_provider_and_quotes_in_paths_are_escaped() {
        let mut b = block(None);
        b.web_url = Some("https://ghe.example.com");
        b.api_url = Some("https://ghe.example.com/api/v3");
        b.private_key_file = "/tmp/a\"b/private-key.pem";
        b.webhook_secret_file = None;
        assert_eq!(
            block_text(&b),
            concat!(
                "[[scm]]\n",
                "id = \"github\"\n",
                "provider = \"github\"\n",
                "web_url = \"https://ghe.example.com\"\n",
                "api_url = \"https://ghe.example.com/api/v3\"\n",
                "app_id = 123456\n",
                "private_key_file = '/tmp/a\"b/private-key.pem'\n",
                "client_id = \"Iv23liAbCdEfGh123456\"\n",
            )
        );
    }

    #[test]
    fn the_block_is_appended_after_a_newline_and_a_blank_line() {
        assert_eq!(appended("", "[x]\n"), "[x]\n");
        assert_eq!(appended("a = 1", "[x]\n"), "a = 1\n\n[x]\n");
        assert_eq!(appended("a = 1\n", "[x]\n"), "a = 1\n\n[x]\n");
    }

    #[test]
    fn a_private_file_is_never_written_over() {
        let dir = tempfile::tempdir().unwrap();
        let at = dir.path().join("scm/github");
        let path = write_new_private_file(&at, PRIVATE_KEY_FILE, b"one").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"one");
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(
            std::fs::metadata(&at).unwrap().permissions().mode() & 0o777,
            0o700
        );
        let again = write_new_private_file(&at, PRIVATE_KEY_FILE, b"two");
        assert!(matches!(again, Err(WriteError::Exists(p)) if p == path));
        assert_eq!(std::fs::read(&path).unwrap(), b"one");
        let names: Vec<_> = std::fs::read_dir(&at).unwrap().collect();
        assert_eq!(names.len(), 1, "no temporary file is left behind");
    }

    #[test]
    fn a_symlinked_config_keeps_its_link_and_mode() {
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("dotfiles/config.toml");
        std::fs::create_dir_all(real.parent().unwrap()).unwrap();
        std::fs::write(&real, "a = 1\n").unwrap();
        std::fs::set_permissions(&real, std::fs::Permissions::from_mode(0o640)).unwrap();
        let link = dir.path().join("config.toml");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        replace_config(&link, "a = 2\n").unwrap();
        assert!(
            std::fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(std::fs::read_to_string(&real).unwrap(), "a = 2\n");
        assert_eq!(
            std::fs::metadata(&real).unwrap().permissions().mode() & 0o777,
            0o640
        );
    }
}
