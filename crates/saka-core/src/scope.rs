use globset::{GlobBuilder, GlobSet, GlobSetBuilder};

pub const MAX_PATTERNS: usize = 64;
pub const MAX_PATTERN_BYTES: usize = 256;
pub const EVERYTHING: &str = "**";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScopeIssue {
    TooMany,
    Pattern(usize),
    NoInclude,
}

#[derive(Debug, Clone)]
pub struct PathScope {
    patterns: Vec<String>,
    include: GlobSet,
    exclude: GlobSet,
}

fn glob(pattern: &str) -> Option<globset::Glob> {
    GlobBuilder::new(pattern)
        .literal_separator(true)
        .backslash_escape(false)
        .build()
        .ok()
}

fn body_ok(body: &str) -> bool {
    !body.is_empty()
        && !body.starts_with('/')
        && !body.contains('\\')
        && body.split('/').all(|s| s != "." && s != "..")
        && glob(body).is_some()
}

pub fn pattern_ok(pattern: &str) -> bool {
    (1..=MAX_PATTERN_BYTES).contains(&pattern.len())
        && !pattern.contains('\0')
        && body_ok(pattern.strip_prefix('!').unwrap_or(pattern))
}

fn is_exclude(pattern: &str) -> bool {
    pattern.starts_with('!')
}

fn set<'a>(patterns: impl Iterator<Item = &'a str>) -> GlobSet {
    let mut builder = GlobSetBuilder::new();
    for p in patterns {
        if let Some(g) = glob(p) {
            builder.add(g);
        }
    }
    builder.build().unwrap_or_else(|_| GlobSet::empty())
}

impl PathScope {
    pub fn parse(patterns: &[String]) -> Result<PathScope, Vec<ScopeIssue>> {
        let mut issues = Vec::new();
        if patterns.len() > MAX_PATTERNS {
            issues.push(ScopeIssue::TooMany);
        }
        for (j, p) in patterns.iter().enumerate() {
            if !pattern_ok(p) {
                issues.push(ScopeIssue::Pattern(j));
            }
        }
        let includes = patterns.iter().any(|p| !p.is_empty() && !is_exclude(p));
        if issues.is_empty() && !includes {
            issues.push(ScopeIssue::NoInclude);
        }
        if !issues.is_empty() {
            return Err(issues);
        }
        Ok(PathScope {
            patterns: patterns.to_vec(),
            include: set(patterns
                .iter()
                .filter(|p| !is_exclude(p))
                .map(String::as_str)),
            exclude: set(patterns.iter().filter_map(|p| p.strip_prefix('!'))),
        })
    }

    pub fn effective(path_scope: &[String], docs_folder: Option<&str>) -> Vec<String> {
        let mut out = path_scope.to_vec();
        if let Some(folder) = docs_folder.map(|f| f.trim_matches('/'))
            && !folder.is_empty()
            && folder != "."
        {
            let docs = format!("{folder}/**");
            if !out.contains(&docs) {
                out.push(docs);
            }
        }
        out
    }

    pub fn patterns(&self) -> &[String] {
        &self.patterns
    }

    pub fn contains(&self, path: &str) -> bool {
        self.include.is_match(path) && !self.exclude.is_match(path)
    }

    pub fn outside(&self, paths: &[String]) -> Vec<String> {
        paths
            .iter()
            .filter(|p| !self.contains(p))
            .cloned()
            .collect()
    }
}

pub fn static_prefix(pattern: &str) -> Vec<&str> {
    pattern
        .split('/')
        .take_while(|s| !s.contains(['*', '?', '[', '{']))
        .collect()
}

fn segment_prefix(a: &[&str], b: &[&str]) -> bool {
    a.len() <= b.len() && a.iter().zip(b).all(|(x, y)| x == y)
}

pub fn overlaps(a: &[String], b: &[String]) -> bool {
    let includes = |s: &[String]| -> Vec<Vec<String>> {
        s.iter()
            .filter(|p| !is_exclude(p))
            .map(|p| static_prefix(p).into_iter().map(String::from).collect())
            .collect()
    };
    let (a, b) = (includes(a), includes(b));
    a.iter().any(|x| {
        b.iter().any(|y| {
            let x: Vec<&str> = x.iter().map(String::as_str).collect();
            let y: Vec<&str> = y.iter().map(String::as_str).collect();
            segment_prefix(&x, &y) || segment_prefix(&y, &x)
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scope(patterns: &[&str]) -> PathScope {
        PathScope::parse(
            &patterns
                .iter()
                .map(|p| (*p).to_string())
                .collect::<Vec<_>>(),
        )
        .unwrap()
    }

    fn strings(patterns: &[&str]) -> Vec<String> {
        patterns.iter().map(|p| (*p).to_string()).collect()
    }

    #[test]
    fn a_directory_glob_matches_below_it_only() {
        let s = scope(&["services/api/**"]);
        assert!(s.contains("services/api/x/y.rs"));
        assert!(s.contains("services/api/a.rs"));
        assert!(!s.contains("services/apix/a"));
        assert!(!s.contains("services/web/a.ts"));
    }

    #[test]
    fn a_star_does_not_cross_a_slash() {
        let s = scope(&["*.md"]);
        assert!(s.contains("README.md"));
        assert!(!s.contains("a/b.md"));
        assert!(scope(&["**"]).contains("a/b/c.md"));
        assert!(scope(&["**/*.md"]).contains("a/b.md"));
    }

    #[test]
    fn an_exclude_wins_over_an_include() {
        let s = scope(&["!services/api/fixtures/**", "services/api/**"]);
        assert!(s.contains("services/api/src/main.rs"));
        assert!(!s.contains("services/api/fixtures/one.json"));
        assert_eq!(
            s.outside(&strings(&[
                "services/api/fixtures/a",
                "services/api/b",
                "web/c"
            ])),
            strings(&["services/api/fixtures/a", "web/c"])
        );
    }

    #[test]
    fn invalid_patterns_are_refused_by_index() {
        let bad = strings(&["/a", "a\\b", "a/../b", "[", "!", "", "./a", "ok/**"]);
        assert_eq!(
            PathScope::parse(&bad).unwrap_err(),
            (0..7).map(ScopeIssue::Pattern).collect::<Vec<_>>()
        );
        assert_eq!(
            PathScope::parse(&strings(&["!a/**"])).unwrap_err(),
            [ScopeIssue::NoInclude]
        );
        assert_eq!(PathScope::parse(&[]).unwrap_err(), [ScopeIssue::NoInclude]);
        let many = vec!["a/**".to_string(); MAX_PATTERNS + 1];
        assert_eq!(PathScope::parse(&many).unwrap_err(), [ScopeIssue::TooMany]);
        assert!(!pattern_ok(&"a".repeat(MAX_PATTERN_BYTES + 1)));
        assert!(!pattern_ok("a\0b"));
    }

    #[test]
    fn the_effective_scope_adds_the_docs_folder_of_the_workspace_repo() {
        let base = strings(&["services/api/**"]);
        assert_eq!(
            PathScope::effective(&base, Some("docs")),
            strings(&["services/api/**", "docs/**"])
        );
        assert_eq!(PathScope::effective(&base, Some(".")), base);
        assert_eq!(PathScope::effective(&base, None), base);
        let s = PathScope::parse(&PathScope::effective(
            &strings(&["services/api/**", "!docs/private/**"]),
            Some("docs"),
        ))
        .unwrap();
        assert!(s.contains("docs/api.md"));
        assert!(!s.contains("docs/private/x.md"));
    }

    #[test]
    fn the_static_prefix_stops_at_the_first_glob_segment() {
        assert_eq!(static_prefix("services/api/**"), ["services", "api"]);
        assert_eq!(static_prefix("services/*.rs"), ["services"]);
        assert_eq!(static_prefix("a/{b,c}/d"), ["a"]);
        assert_eq!(static_prefix("pnpm-lock.yaml"), ["pnpm-lock.yaml"]);
        assert!(static_prefix("**").is_empty());
    }

    #[test]
    fn scopes_overlap_when_one_static_prefix_contains_the_other() {
        let api = strings(&["services/api/**"]);
        assert!(overlaps(&api, &strings(&["services/**"])));
        assert!(overlaps(&strings(&["services/**"]), &api));
        assert!(!overlaps(&api, &strings(&["services/web/**"])));
        assert!(overlaps(&api, &strings(&["**"])));
        assert!(!overlaps(
            &strings(&["pnpm-lock.yaml"]),
            &strings(&["services/**"])
        ));
        assert!(!overlaps(
            &strings(&["services/api/**", "!services/**"]),
            &strings(&["services/web/**"])
        ));
        assert!(overlaps(&strings(&["a/*.ts"]), &strings(&["a/*.go"])));
    }
}
