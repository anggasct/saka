//! Workspace paths: the project folder and spec files inside it.

/// Why a path is not a workspace path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PathIssue {
    /// Empty, absolute, with an empty, `.` or `..` segment, or with a character outside
    /// `A-Za-z0-9._-`.
    Invalid,
    /// Not inside the project folder.
    OutsideProject,
    /// Not a markdown file.
    NotMarkdown,
}

fn segments_ok(path: &str) -> bool {
    !path.is_empty()
        && path.split('/').all(|seg| {
            !seg.is_empty()
                && seg != "."
                && seg != ".."
                && seg
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
        })
}

/// Checks a project folder: `.` for the repo root, or a relative path.
pub fn check_project_path(project_path: &str) -> Result<(), PathIssue> {
    if project_path == "." || segments_ok(project_path) {
        Ok(())
    } else {
        Err(PathIssue::Invalid)
    }
}

/// Checks a spec path: relative to the repo root, inside `project_path`, ending in `.md`.
pub fn check_spec_path(project_path: &str, path: &str) -> Result<(), PathIssue> {
    check_project_path(project_path)?;
    if !segments_ok(path) {
        return Err(PathIssue::Invalid);
    }
    if project_path != "." && !path.starts_with(&format!("{project_path}/")) {
        return Err(PathIssue::OutsideProject);
    }
    if !path.ends_with(".md") {
        return Err(PathIssue::NotMarkdown);
    }
    Ok(())
}

/// `saka.toml` of a project folder.
pub fn project_config_path(project_path: &str) -> String {
    if project_path == "." {
        "saka.toml".to_string()
    } else {
        format!("{project_path}/saka.toml")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spec_paths_stay_inside_the_project_folder() {
        assert_eq!(check_spec_path("docs", "docs/specs/a.md"), Ok(()));
        assert_eq!(check_spec_path(".", "specs/a.md"), Ok(()));
        assert_eq!(
            check_spec_path("docs", "src/a.md"),
            Err(PathIssue::OutsideProject)
        );
        assert_eq!(
            check_spec_path("docs", "docsx/a.md"),
            Err(PathIssue::OutsideProject)
        );
        assert_eq!(
            check_spec_path("docs", "docs/a.txt"),
            Err(PathIssue::NotMarkdown)
        );
        for bad in [
            "",
            "/docs/a.md",
            "docs/../a.md",
            "docs//a.md",
            "docs/./a.md",
            "docs/a b.md",
        ] {
            assert_eq!(
                check_spec_path("docs", bad),
                Err(PathIssue::Invalid),
                "{bad}"
            );
        }
        assert_eq!(check_project_path("docs/../x"), Err(PathIssue::Invalid));
        assert_eq!(project_config_path("."), "saka.toml");
        assert_eq!(project_config_path("docs"), "docs/saka.toml");
    }
}
