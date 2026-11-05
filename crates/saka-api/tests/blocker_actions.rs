#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "tests fail loudly on a broken setup"
)]

use std::collections::BTreeMap;
use std::path::PathBuf;

use saka_core::blocker::designated;
use saka_core::vocab::{BlockerAction, BlockerCode};

fn catalog(locale: &str) -> BTreeMap<String, String> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../web/src/i18n")
        .join(format!("{locale}.json"));
    let text = std::fs::read_to_string(&path).expect("the web catalog is part of the repo");
    serde_json::from_str(&text).expect("the catalog is a flat JSON object")
}

#[test]
fn every_blocker_code_has_a_sentence_in_every_language() {
    for locale in ["en", "id"] {
        let catalog = catalog(locale);
        for code in BlockerCode::ALL {
            let key = format!("blocker.{code}");
            assert!(
                catalog.get(&key).is_some_and(|s| !s.is_empty()),
                "{locale} has no sentence {key}"
            );
        }
    }
}

#[test]
fn a_docs_variant_of_a_sentence_exists_in_every_language_or_in_none() {
    let (en, id) = (catalog("en"), catalog("id"));
    for code in BlockerCode::ALL {
        let key = format!("blocker.{code}.docs");
        assert_eq!(en.contains_key(&key), id.contains_key(&key), "{key}");
    }
}

#[test]
fn every_action_kind_has_a_label_in_every_language() {
    for locale in ["en", "id"] {
        let catalog = catalog(locale);
        for kind in BlockerAction::ALL {
            let key = format!("blocker.action.{kind}");
            assert!(
                catalog.get(&key).is_some_and(|s| !s.is_empty()),
                "{locale} has no label {key}"
            );
        }
    }
}

#[test]
fn every_action_kind_is_the_primary_action_of_some_code() {
    for kind in BlockerAction::ALL {
        assert!(
            BlockerCode::ALL.iter().any(|c| designated(*c) == *kind),
            "no code uses {kind}"
        );
    }
}
