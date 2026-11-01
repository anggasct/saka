//! The parsed spec model stored with each revision.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpecModel {
    pub slug: String,
    pub title: String,
    pub acceptance_criteria: Vec<AcceptanceCriterion>,
    pub steps: Vec<Step>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AcceptanceCriterion {
    /// `AC-01`, unique in the spec.
    pub id: String,
    pub text: String,
    pub line: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Step {
    /// `S1`, `S2`, … in order.
    pub id: String,
    /// 1-based position.
    pub index: u32,
    pub work: String,
    /// Acceptance criterion ids this step covers.
    pub ac: Vec<String>,
    /// Commands from the "Done when" column; each must pass in the task environment.
    pub commands: Vec<String>,
    pub line: u32,
}
