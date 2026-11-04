use serde::{Deserialize, Serialize};

pub const MIN_PRIORITY: i32 = -1000;
pub const MAX_PRIORITY: i32 = 1000;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpecModel {
    pub slug: String,
    pub title: String,
    #[serde(default)]
    pub priority: i32,
    #[serde(default)]
    pub depends_on: Vec<String>,
    pub acceptance_criteria: Vec<AcceptanceCriterion>,
    pub steps: Vec<Step>,
    pub links: Vec<Link>,
    #[serde(default)]
    pub decisions: Vec<Decision>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Decision {
    pub id: String,
    pub question: String,
    pub line: u32,
    pub options: Vec<DecisionOption>,
    pub chosen: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DecisionOption {
    pub text: String,
    pub line: u32,
    pub recommended: bool,
    pub chosen: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Link {
    pub target: String,
    pub line: u32,
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
    #[serde(default)]
    pub after: Vec<String>,
    #[serde(default)]
    pub repo: Option<String>,
    pub line: u32,
}
