//! Spec parser and structure lint. Pure: no I/O, async, clock, or randomness.
//!
//! A spec is a markdown file marked by front matter `saka: spec`. Only three parts have a
//! fixed shape: acceptance criteria with ids, a Steps table, and the commands that prove
//! each step. Everything else is free text.

mod lint;
mod model;
mod parse;

pub use lint::{LintCode, LintIssue};
pub use model::{AcceptanceCriterion, SpecModel, Step};
pub use parse::{MAX_SPEC_BYTES, MAX_STEPS, PARSER_VERSION, Parsed, parse};
