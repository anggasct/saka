#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "tests fail loudly on a broken setup"
)]

use proptest::prelude::*;
use saka_core::scope::PathScope;

const PATTERNS: &[&str] = &[
    "services/api/**",
    "services/**",
    "!services/api/fixtures/**",
    "*.md",
    "!**/*.lock",
    "docs/**",
    "!docs/private/**",
    "pnpm-lock.yaml",
    "web/{src,test}/**",
    "!web/src/gen/*.ts",
];

const PATHS: &[&str] = &[
    "services/api/src/main.rs",
    "services/api/fixtures/a.json",
    "services/apix/a",
    "services/web/a.ts",
    "README.md",
    "a/b.md",
    "Cargo.lock",
    "services/api/Cargo.lock",
    "docs/api.md",
    "docs/private/key.md",
    "pnpm-lock.yaml",
    "web/src/app.ts",
    "web/src/gen/schema.ts",
    "web/test/app.test.ts",
];

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    #[test]
    fn the_order_of_patterns_never_changes_what_is_in_scope(
        (patterns, permuted) in prop::sample::subsequence(PATTERNS.to_vec(), 1..=PATTERNS.len())
            .prop_flat_map(|v| (Just(v.clone()), Just(v).prop_shuffle())),
    ) {
        let patterns: Vec<String> = patterns.iter().map(|p| (*p).to_string()).collect();
        let permuted: Vec<String> = permuted.iter().map(|p| (*p).to_string()).collect();
        let Ok(scope) = PathScope::parse(&patterns) else {
            prop_assert!(patterns.iter().all(|p| p.starts_with('!')));
            return Ok(());
        };
        let other = PathScope::parse(&permuted).unwrap();
        let paths: Vec<String> = PATHS.iter().map(|p| (*p).to_string()).collect();
        prop_assert_eq!(scope.outside(&paths), other.outside(&paths));
        for path in &paths {
            let excluded = patterns
                .iter()
                .filter_map(|p| p.strip_prefix('!'))
                .any(|p| PathScope::parse(&[p.to_string()]).unwrap().contains(path));
            if excluded {
                prop_assert!(!scope.contains(path), "{} is excluded", path);
            }
        }
    }
}
