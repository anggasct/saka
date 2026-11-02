#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PathIssue {
    Invalid,
    OutsideProject,
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

pub fn check_project_path(project_path: &str) -> Result<(), PathIssue> {
    if project_path == "." || segments_ok(project_path) {
        Ok(())
    } else {
        Err(PathIssue::Invalid)
    }
}

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

pub const MAX_PATH_BYTES: usize = 1024;
pub const MAX_DOC_BYTES: u64 = 1024 * 1024;
pub const MAX_TREE_FILES: usize = 5000;
pub const MAX_SEGMENT_BYTES: usize = 255;
const DOC_EXTENSIONS: [&str; 2] = [".md", ".markdown"];

fn doc_segment_ok(seg: &str) -> bool {
    !seg.is_empty()
        && seg.len() <= MAX_SEGMENT_BYTES
        && seg != "."
        && seg != ".."
        && !seg.eq_ignore_ascii_case(".git")
        && seg.bytes().all(|b| b >= 0x20 && b != 0x7f && b != b'\\')
}

pub fn is_doc_file_name(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    DOC_EXTENSIONS.iter().any(|ext| lower.ends_with(ext))
}

pub fn is_inside_project(project_path: &str, path: &str) -> bool {
    project_path == "." || path.starts_with(&format!("{project_path}/"))
}

pub fn check_doc_path(project_path: &str, path: &str) -> Result<(), PathIssue> {
    check_project_path(project_path)?;
    if path.len() > MAX_PATH_BYTES || !path.split('/').all(doc_segment_ok) {
        return Err(PathIssue::Invalid);
    }
    if !is_inside_project(project_path, path) {
        return Err(PathIssue::OutsideProject);
    }
    if !is_doc_file_name(path) {
        return Err(PathIssue::NotMarkdown);
    }
    Ok(())
}

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

    #[test]
    fn doc_paths_allow_any_readable_name_inside_the_project_folder() {
        for ok in [
            "docs/a.md",
            "docs/specs/B.MD",
            "docs/notes/My Notes (draft).markdown",
            "docs/catatan/rencana-ü.md",
            "docs/.hidden/a.md",
        ] {
            assert_eq!(check_doc_path("docs", ok), Ok(()), "{ok}");
        }
        assert_eq!(check_doc_path(".", "README.md"), Ok(()));
        assert_eq!(
            check_doc_path("docs", "src/a.md"),
            Err(PathIssue::OutsideProject)
        );
        assert_eq!(
            check_doc_path("docs", "docsx/a.md"),
            Err(PathIssue::OutsideProject)
        );
        assert_eq!(
            check_doc_path("docs", "docs/a.txt"),
            Err(PathIssue::NotMarkdown)
        );
        let long = format!("docs/{}.md", "a".repeat(MAX_SEGMENT_BYTES));
        let deep = format!("docs/{}a.md", "abcdefgh/".repeat(128));
        for bad in [
            "",
            "/docs/a.md",
            "docs/../a.md",
            "docs//a.md",
            "docs/./a.md",
            "docs/.git/a.md",
            "docs/.GIT/a.md",
            "docs/a\\b.md",
            "docs/a\tb.md",
            "docs/a\u{7f}.md",
            "docs/",
            long.as_str(),
            deep.as_str(),
        ] {
            assert_eq!(
                check_doc_path("docs", bad),
                Err(PathIssue::Invalid),
                "{bad}"
            );
        }
    }
}
