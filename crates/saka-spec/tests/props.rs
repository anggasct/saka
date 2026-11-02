use proptest::prelude::*;
use saka_spec::parse;

const VALID: &str = include_str!("fixtures/after-graph.md");
const MARKUP: &[u8] = b"|-:`#\n, S";

fn mutated() -> impl Strategy<Value = Vec<u8>> {
    let edits = prop::collection::vec(
        (any::<prop::sample::Index>(), 0usize..4, any::<u8>()),
        0..12,
    );
    edits.prop_map(|edits| {
        let mut bytes = VALID.as_bytes().to_vec();
        for (at, kind, byte) in edits {
            let i = at.index(bytes.len().max(1));
            match kind {
                0 if !bytes.is_empty() => {
                    bytes.remove(i.min(bytes.len() - 1));
                }
                1 => bytes.insert(i.min(bytes.len()), byte),
                2 => bytes.insert(i.min(bytes.len()), MARKUP[usize::from(byte) % MARKUP.len()]),
                _ if !bytes.is_empty() => {
                    let len = bytes.len();
                    bytes[i.min(len - 1)] = byte;
                }
                _ => {}
            }
        }
        bytes
    })
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    #[test]
    fn random_bytes_never_panic_and_parse_the_same(bytes in prop::collection::vec(any::<u8>(), 0..2048)) {
        prop_assert_eq!(parse("x.md", &bytes), parse("x.md", &bytes));
    }

    #[test]
    fn link_heavy_text_never_panics_and_stays_under_the_link_limit(links in prop::collection::vec(
        prop_oneof![
            "\\[[a-z]{0,3}\\]\\([ -~]{0,12}\\)",
            "\\[[a-z]\\]: [ -~]{0,12}",
            "%[0-9a-fA-F]{0,2}",
            "[ -~]{0,10}",
        ],
        0..400,
    )) {
        let text = format!("---\nsaka: spec\n---\n# T\n\n{}\n", links.join(" \n"));
        let parsed = parse("docs/x.md", text.as_bytes());
        prop_assert_eq!(&parsed, &parse("docs/x.md", text.as_bytes()));
        let count = parsed.model.map_or(0, |m| m.links.len());
        prop_assert!(count <= saka_spec::context::MAX_LINKS);
    }

    #[test]
    fn mutated_specs_never_panic_and_parse_the_same(bytes in mutated()) {
        prop_assert_eq!(parse("after-graph.md", &bytes), parse("after-graph.md", &bytes));
    }

    #[test]
    fn spec_shaped_text_never_panics(lines in prop::collection::vec(
        prop_oneof![
            Just("---".to_string()),
            Just("saka: spec".to_string()),
            Just("depends_on:".to_string()),
            Just("  - a".to_string()),
            Just("priority: 5".to_string()),
            Just("## Steps".to_string()),
            Just("## Done when".to_string()),
            Just("## Acceptance criteria".to_string()),
            Just("- **AC-01**: x".to_string()),
            Just("| Step | Work | AC | After | Done when |".to_string()),
            Just("|---|---|---|---|---|".to_string()),
            "\\| S[0-9] \\| [a-z]* \\| AC-0[0-9] \\| (S[0-9](, S[0-9])?)? \\| `[a-z]+` \\|",
            "- `[a-z ]*`( `[a-z]`)?",
            Just("## Decisions".to_string()),
            "#{1,4} Q[0-9]{1,3}:[ \\t]?[ -~]{0,8}",
            "[ ]{0,4}([-*]|[0-9]\\.)[ \\t]\\[[ xX]?\\][ \\t]?[ -~]{0,16}( \\(recommended\\))?",
            Just("\r".to_string()),
            "[ -~]{0,20}",
        ],
        0..30,
    )) {
        let text = lines.join("\n");
        prop_assert_eq!(parse("x.md", text.as_bytes()), parse("x.md", text.as_bytes()));
    }
}
