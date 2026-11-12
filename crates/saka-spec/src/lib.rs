mod content;
pub mod context;
mod decision;
mod frontmatter;
mod graph;
mod lint;
mod model;
mod parse;
mod quick;
mod rules;
mod template;

pub use content::{content_hash, is_spec};
pub use decision::MAX_DECISIONS;
pub use lint::{LintCode, LintIssue, Severity};
pub use model::{
    AcceptanceCriterion, Decision, DecisionOption, Link, MAX_PRIORITY, MIN_PRIORITY, SpecModel,
    Step,
};
pub use parse::{MAX_REGRESSION, MAX_SPEC_BYTES, MAX_STEPS, PARSER_VERSION, Parsed, parse};
pub use quick::{
    MAX_QUICK_COMMAND, MAX_QUICK_COMMANDS, MAX_QUICK_CRITERIA, MAX_QUICK_CRITERION,
    MAX_QUICK_DESCRIPTION, MAX_QUICK_TITLE, QuickIssue, QuickSpec, check as check_quick,
    keeps_structure as quick_keeps_structure, render as render_quick, slug as quick_slug,
};
pub use rules::rules;
pub use saka_core::vocab::WorkKind;
pub use template::{Locale, template};
