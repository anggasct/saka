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
    pub id: String,
    pub text: String,
    pub line: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Step {
    pub id: String,
    pub index: u32,
    pub work: String,
    pub ac: Vec<String>,
    pub commands: Vec<String>,
    pub line: u32,
}
