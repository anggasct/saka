use std::io::{Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::{Component, Path, PathBuf};

use saka_core::workspace::MAX_BINDINGS;
use saka_engine::project_config::{self, Issue};
use saka_engine::versions::{self, FileUse, RepoFiles};
use serde::Serialize;
use serde_json::{Value, json};
use toml_edit::{DocumentMut, value};

use super::{EXIT_PROBLEMS, InitError};

const FILE_MODE: u32 = 0o644;
const WORKFLOW_DIR: &str = ".github/workflows";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ConfigAction {
    Create,
    Update,
    None,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Note {
    pub code: &'static str,
    pub params: Value,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Binding {
    pub name: String,
    pub url: String,
    pub base_branch: Option<String>,
    pub path_scope: Vec<String>,
    pub make_default: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Planned {
    pub action: ConfigAction,
    pub content: String,
    pub repo_name: String,
    pub notes: Vec<Note>,
}

fn quoted(text: &str) -> String {
    toml_edit::Value::from(text).to_string().trim().to_string()
}

fn repo_table(binding: &Binding, default: bool) -> String {
    let mut text = format!(
        "[[repo]]\nname = {}\nurl = {}\n",
        quoted(&binding.name),
        quoted(&binding.url)
    );
    if default {
        text.push_str("default = true\n");
    }
    if let Some(branch) = &binding.base_branch {
        text.push_str(&format!("base_branch = {}\n", quoted(branch)));
    }
    if !binding.path_scope.is_empty() {
        let items: Vec<String> = binding.path_scope.iter().map(|p| quoted(p)).collect();
        text.push_str(&format!("path_scope = [{}]\n", items.join(", ")));
    }
    text
}

fn invalid_result(issues: &[Issue]) -> InitError {
    InitError::new(
        EXIT_PROBLEMS,
        "config_invalid",
        json!({ "issues": project_config::issues_json(issues) }),
    )
}

fn checked(content: String) -> Result<String, InitError> {
    project_config::parse(content.as_bytes()).map_err(|issues| invalid_result(&issues))?;
    Ok(content)
}

pub fn create(project_name: &str, binding: &Binding) -> Result<Planned, InitError> {
    let content = format!(
        "[project]\nname = {}\n\n{}",
        quoted(project_name),
        repo_table(binding, false)
    );
    Ok(Planned {
        action: ConfigAction::Create,
        content: checked(content)?,
        repo_name: binding.name.clone(),
        notes: Vec::new(),
    })
}

fn set_default(table: &mut toml_edit::Table, on: bool) {
    match table.get_mut("default").and_then(|i| i.as_value_mut()) {
        Some(existing) => {
            let decor = existing.decor().clone();
            *existing = toml_edit::Value::from(on);
            *existing.decor_mut() = decor;
        }
        None => {
            table.insert("default", value(on));
        }
    }
}

pub fn merge(existing: &str, binding: &Binding) -> Result<Planned, InitError> {
    let config = project_config::parse(existing.as_bytes()).map_err(|issues| {
        InitError::new(
            EXIT_PROBLEMS,
            "existing_config_invalid",
            json!({ "issues": project_config::issues_json(&issues) }),
        )
    })?;
    let normalized = super::git::normalize(&binding.url);
    let bound = config.repos.iter().find(|r| {
        normalized
            .as_ref()
            .and_then(|n| n.url.strip_prefix("https://"))
            .and_then(|rest| rest.split_once('/'))
            .is_some_and(|(host, full)| r.same_repo(host, full))
    });
    if let Some(entry) = bound {
        let mut fields = Vec::new();
        if binding.base_branch.is_some() && binding.base_branch != entry.base_branch {
            fields.push("base_branch");
        }
        if !binding.path_scope.is_empty() && binding.path_scope != entry.path_scope {
            fields.push("path_scope");
        }
        if binding.make_default && !entry.is_default {
            fields.push("default");
        }
        let mut notes = vec![Note {
            code: "repo_already_bound",
            params: json!({ "name": entry.name }),
        }];
        if !fields.is_empty() {
            notes.push(Note {
                code: "repo_binding_differs",
                params: json!({ "name": entry.name, "fields": fields }),
            });
        }
        return Ok(Planned {
            action: ConfigAction::None,
            content: existing.to_string(),
            repo_name: entry.name.clone(),
            notes,
        });
    }
    if config.repos.iter().any(|r| r.name == binding.name) {
        return Err(InitError::failed(
            "repo_name_taken",
            json!({ "name": binding.name }),
        ));
    }
    if config.repos.len() >= MAX_BINDINGS {
        return Err(InitError::new(
            EXIT_PROBLEMS,
            "too_many_repos",
            json!({ "max": MAX_BINDINGS }),
        ));
    }
    let mut doc: DocumentMut = existing.parse().map_err(|_| {
        InitError::new(
            EXIT_PROBLEMS,
            "existing_config_invalid",
            json!({ "issues": [] }),
        )
    })?;
    let mut flipped = false;
    if let Some(tables) = doc
        .get_mut("repo")
        .and_then(toml_edit::Item::as_array_of_tables_mut)
    {
        let single = tables.len() == 1;
        for table in tables.iter_mut() {
            let is_default = table.get("default").and_then(toml_edit::Item::as_bool) == Some(true);
            if binding.make_default && is_default {
                set_default(table, false);
                flipped = true;
            } else if single && !binding.make_default && table.get("default").is_none() {
                set_default(table, true);
                flipped = true;
            }
        }
    }
    let mut content = if flipped {
        doc.to_string()
    } else {
        existing.to_string()
    };
    if !content.is_empty() && !content.ends_with('\n') {
        content.push('\n');
    }
    content.push('\n');
    content.push_str(&repo_table(binding, binding.make_default));
    Ok(Planned {
        action: ConfigAction::Update,
        content: checked(content)?,
        repo_name: binding.name.clone(),
        notes: Vec::new(),
    })
}

fn write_failed(path: &Path, e: &std::io::Error) -> InitError {
    InitError::failed(
        "file_write_failed",
        json!({ "path": path.display().to_string(), "reason": e.to_string() }),
    )
}

fn relative_ok(rel: &str) -> bool {
    !rel.is_empty()
        && Path::new(rel)
            .components()
            .all(|c| matches!(c, Component::Normal(_)))
}

pub fn target(root: &Path, rel: &str) -> Result<PathBuf, InitError> {
    if !relative_ok(rel) {
        return Err(InitError::failed(
            "path_outside_repo",
            json!({ "path": rel }),
        ));
    }
    Ok(root.join(rel))
}

// Checked on the deepest folder that exists, before and after creating the rest,
// so a symlinked folder cannot move the write (or a new folder) outside the clone.
fn inside(root: &Path, dir: &Path) -> bool {
    let Ok(root) = root.canonicalize() else {
        return false;
    };
    let existing = dir.ancestors().find(|a| a.exists());
    existing
        .and_then(|a| a.canonicalize().ok())
        .is_some_and(|a| a.starts_with(root))
}

pub fn write_atomic(root: &Path, rel: &str, bytes: &[u8], replace: bool) -> Result<(), InitError> {
    let path = target(root, rel)?;
    let parent = path
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| root.to_path_buf());
    if !inside(root, &parent) {
        return Err(InitError::failed(
            "path_outside_repo",
            json!({ "path": rel }),
        ));
    }
    std::fs::create_dir_all(&parent).map_err(|e| write_failed(&path, &e))?;
    if !inside(root, &parent) {
        return Err(InitError::failed(
            "path_outside_repo",
            json!({ "path": rel }),
        ));
    }
    match std::fs::symlink_metadata(&path) {
        Ok(meta) if !meta.is_file() || !replace => {
            return Err(InitError::failed("file_exists", json!({ "path": rel })));
        }
        _ => {}
    }
    let mut temp = tempfile::Builder::new()
        .prefix(".saka-init-")
        .permissions(std::fs::Permissions::from_mode(FILE_MODE))
        .tempfile_in(&parent)
        .map_err(|e| write_failed(&path, &e))?;
    temp.write_all(bytes).map_err(|e| write_failed(&path, &e))?;
    temp.as_file()
        .sync_all()
        .map_err(|e| write_failed(&path, &e))?;
    temp.persist(&path)
        .map_err(|e| write_failed(&path, &e.error))?;
    if let Ok(dir) = std::fs::File::open(&parent) {
        let _ = dir.sync_all();
    }
    Ok(())
}

pub fn read_regular(path: &Path, max: usize) -> std::io::Result<Option<Vec<u8>>> {
    let meta = match std::fs::symlink_metadata(path) {
        Ok(meta) => meta,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    };
    if !meta.is_file() {
        return Ok(None);
    }
    let file = std::fs::File::open(path)?;
    let mut bytes = Vec::new();
    let limit = u64::try_from(max).unwrap_or(u64::MAX).saturating_add(1);
    file.take(limit).read_to_end(&mut bytes)?;
    Ok(Some(bytes))
}

fn regular_names(dir: &Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut names: Vec<String> = entries
        .filter_map(Result::ok)
        .filter(|e| e.file_type().is_ok_and(|t| t.is_file()))
        .filter_map(|e| e.file_name().into_string().ok())
        .collect();
    names.sort();
    names
}

pub fn collect(root: &Path) -> RepoFiles {
    let mut files = RepoFiles::new();
    let mut paths: Vec<String> = regular_names(root);
    paths.extend(
        regular_names(&root.join(WORKFLOW_DIR))
            .into_iter()
            .map(|n| format!("{WORKFLOW_DIR}/{n}")),
    );
    paths.push(saka_engine::recipe::RECIPE_PATH.to_string());
    for path in paths {
        match versions::file_use(&path) {
            Some(FileUse::Content) => {
                if let Ok(Some(bytes)) = read_regular(&root.join(&path), versions::MAX_FILE_BYTES) {
                    files.add(&path, bytes);
                }
            }
            Some(FileUse::Presence) => files.add_present(&path),
            None => {}
        }
    }
    files
}

pub fn single_line(bytes: &[u8]) -> bool {
    String::from_utf8_lossy(bytes)
        .lines()
        .filter(|l| !l.trim().is_empty())
        .count()
        <= 1
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shop() -> Binding {
        Binding {
            name: "shop".into(),
            url: "https://github.com/acme/shop".into(),
            base_branch: None,
            path_scope: Vec::new(),
            make_default: false,
        }
    }

    #[test]
    fn a_new_config_is_byte_exact() {
        let planned = create("shop", &shop()).unwrap();
        assert_eq!(
            planned.content,
            "[project]\nname = \"shop\"\n\n[[repo]]\nname = \"shop\"\nurl = \"https://github.com/acme/shop\"\n"
        );
        let mut scoped = shop();
        scoped.base_branch = Some("develop".into());
        scoped.path_scope = vec!["services/api/**".into(), "!services/api/fixtures/**".into()];
        assert_eq!(
            create("Shop app", &scoped).unwrap().content,
            "[project]\nname = \"Shop app\"\n\n[[repo]]\nname = \"shop\"\nurl = \"https://github.com/acme/shop\"\nbase_branch = \"develop\"\npath_scope = [\"services/api/**\", \"!services/api/fixtures/**\"]\n"
        );
        let mut excludes_only = shop();
        excludes_only.path_scope = vec!["!a/**".into()];
        assert_eq!(
            create("shop", &excludes_only).unwrap_err().exit,
            EXIT_PROBLEMS
        );
    }

    const COMMENTED: &str = "# Shop project\n[project]\nname = \"shop\"   # shown in the app\n\n[[repo]]\n# the API\nname = \"api\"\nurl = \"https://github.com/acme/api\"\npath_scope = [\"**\"]  # all of it\n";

    fn web(make_default: bool) -> Binding {
        Binding {
            name: "web".into(),
            url: "https://github.com/acme/web".into(),
            base_branch: None,
            path_scope: Vec::new(),
            make_default,
        }
    }

    #[test]
    fn merging_appends_the_repo_and_keeps_every_other_byte() {
        let planned = merge(COMMENTED, &web(false)).unwrap();
        assert_eq!(planned.action, ConfigAction::Update);
        let want = "# Shop project\n[project]\nname = \"shop\"   # shown in the app\n\n[[repo]]\n# the API\nname = \"api\"\nurl = \"https://github.com/acme/api\"\npath_scope = [\"**\"]  # all of it\ndefault = true\n\n[[repo]]\nname = \"web\"\nurl = \"https://github.com/acme/web\"\n";
        assert_eq!(planned.content, want);

        let moved = merge(COMMENTED, &web(true)).unwrap();
        assert_eq!(
            moved.content,
            format!(
                "{COMMENTED}\n[[repo]]\nname = \"web\"\nurl = \"https://github.com/acme/web\"\ndefault = true\n"
            )
        );

        let two = merge(
            &planned.content,
            &Binding {
                name: "docs".into(),
                url: "https://github.com/acme/docs".into(),
                ..web(false)
            },
        )
        .unwrap();
        assert!(two.content.starts_with(&planned.content), "{}", two.content);

        let three = "[project]\nname = \"x\"\n\n[[repo]]\nname = \"a\"\nurl = \"https://github.com/acme/a\"\ndefault = true # main one\n\n[[repo]]\nname = \"b\"\nurl = \"https://github.com/acme/b\"\n";
        let moved = merge(three, &web(true)).unwrap();
        assert_eq!(
            moved.content,
            "[project]\nname = \"x\"\n\n[[repo]]\nname = \"a\"\nurl = \"https://github.com/acme/a\"\ndefault = false # main one\n\n[[repo]]\nname = \"b\"\nurl = \"https://github.com/acme/b\"\n\n[[repo]]\nname = \"web\"\nurl = \"https://github.com/acme/web\"\ndefault = true\n"
        );
        let kept = merge(three, &web(false)).unwrap();
        assert!(kept.content.starts_with(three));
        for result in [&planned, &moved, &kept, &two] {
            assert!(
                project_config::parse(result.content.as_bytes()).is_ok(),
                "{}",
                result.content
            );
        }
    }

    #[test]
    fn a_repo_that_is_already_bound_is_left_alone() {
        let same = Binding {
            name: "other".into(),
            url: "https://GitHub.com/acme/API".into(),
            base_branch: Some("develop".into()),
            ..web(true)
        };
        let planned = merge(COMMENTED, &same).unwrap();
        assert_eq!(planned.action, ConfigAction::None);
        assert_eq!(planned.content, COMMENTED);
        assert_eq!(planned.repo_name, "api");
        assert_eq!(
            planned.notes,
            vec![
                Note {
                    code: "repo_already_bound",
                    params: json!({ "name": "api" })
                },
                Note {
                    code: "repo_binding_differs",
                    params: json!({ "name": "api", "fields": ["base_branch"] })
                },
            ]
        );
    }

    #[test]
    fn merge_refusals_have_their_exit_codes() {
        let taken = Binding {
            name: "api".into(),
            ..web(false)
        };
        let e = merge(COMMENTED, &taken).unwrap_err();
        assert_eq!((e.exit, e.code), (1, "repo_name_taken"));

        let mut full = "[project]\nname = \"x\"\n".to_string();
        for n in 0..MAX_BINDINGS {
            full.push_str(&format!(
                "\n[[repo]]\nname = \"r{n}\"\nurl = \"https://github.com/acme/r{n}\"\n{}",
                if n == 0 { "default = true\n" } else { "" }
            ));
        }
        let e = merge(&full, &web(false)).unwrap_err();
        assert_eq!((e.exit, e.code), (EXIT_PROBLEMS, "too_many_repos"));

        let e = merge("[project]\nname = \"x\"\n", &web(false)).unwrap_err();
        assert_eq!((e.exit, e.code), (EXIT_PROBLEMS, "existing_config_invalid"));
    }

    #[test]
    fn a_file_without_a_final_newline_still_gets_a_separate_table() {
        let text = "[project]\nname = \"x\"\n\n[[repo]]\nname = \"a\"\nurl = \"https://github.com/acme/a\"\ndefault = true";
        let planned = merge(text, &web(false)).unwrap();
        assert_eq!(
            planned.content,
            format!("{text}\n\n[[repo]]\nname = \"web\"\nurl = \"https://github.com/acme/web\"\n")
        );
    }

    #[test]
    fn relative_targets_never_leave_the_root() {
        for bad in ["", "../x", "/etc/passwd", "a/../../b", "./x"] {
            assert!(target(Path::new("/r"), bad).is_err(), "{bad}");
        }
        assert!(target(Path::new("/r"), "docs/saka.toml").is_ok());
    }
}
