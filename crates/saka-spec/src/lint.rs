use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    #[default]
    Error,
    Warning,
}

impl Severity {
    pub fn as_str(self) -> &'static str {
        match self {
            Severity::Error => "error",
            Severity::Warning => "warning",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LintCode {
    NotASpec,
    FileTooLarge,
    InvalidUtf8,
    SlugInvalid,
    TitleMissing,
    AcSectionMissing,
    AcIdInvalid,
    AcIdDuplicate,
    StepsMissing,
    StepsColumnMissing,
    StepIdInvalid,
    StepLimitExceeded,
    StepAcEmpty,
    StepAcUnknown,
    StepCommandMissing,
    StepRepoInvalid,
    StepRepoUnknown,
    FrontmatterInvalid,
    PriorityInvalid,
    DependsOnInvalid,
    DependsOnSelf,
    StepAfterUnknown,
    StepAfterCycle,
    DoneWhenWithSteps,
    DoneWhenItemInvalid,
    AcUncovered,
    StepWorkEmpty,
    StepCommandDuplicate,
    StepParallelStart,
    DependsOnUnknown,
    DependsOnWithdrawn,
    DependsOnCycle,
    LinksLimitExceeded,
    ReferenceOutsideWorkspace,
    ReferenceNotDoc,
    ReferenceMissing,
    ReferenceNotText,
    ReferenceTooLarge,
    ContextTooLarge,
    DecisionIdInvalid,
    DecisionIdDuplicate,
    DecisionLimitExceeded,
    DecisionOptionsInvalid,
    DecisionRecommendationInvalid,
    DecisionAnswerMultiple,
    DecisionUnanswered,
    KindInvalid,
    RegressionMissing,
    RegressionCommandInvalid,
    RegressionTooMany,
    RegressionIgnored,
    RegressionMultiStep,
}

impl LintCode {
    pub const ALL: [LintCode; 52] = [
        LintCode::NotASpec,
        LintCode::FileTooLarge,
        LintCode::InvalidUtf8,
        LintCode::SlugInvalid,
        LintCode::TitleMissing,
        LintCode::AcSectionMissing,
        LintCode::AcIdInvalid,
        LintCode::AcIdDuplicate,
        LintCode::StepsMissing,
        LintCode::StepsColumnMissing,
        LintCode::StepIdInvalid,
        LintCode::StepLimitExceeded,
        LintCode::StepAcEmpty,
        LintCode::StepAcUnknown,
        LintCode::StepCommandMissing,
        LintCode::StepRepoInvalid,
        LintCode::StepRepoUnknown,
        LintCode::FrontmatterInvalid,
        LintCode::PriorityInvalid,
        LintCode::DependsOnInvalid,
        LintCode::DependsOnSelf,
        LintCode::StepAfterUnknown,
        LintCode::StepAfterCycle,
        LintCode::DoneWhenWithSteps,
        LintCode::DoneWhenItemInvalid,
        LintCode::AcUncovered,
        LintCode::StepWorkEmpty,
        LintCode::StepCommandDuplicate,
        LintCode::StepParallelStart,
        LintCode::DependsOnUnknown,
        LintCode::DependsOnWithdrawn,
        LintCode::DependsOnCycle,
        LintCode::LinksLimitExceeded,
        LintCode::ReferenceOutsideWorkspace,
        LintCode::ReferenceNotDoc,
        LintCode::ReferenceMissing,
        LintCode::ReferenceNotText,
        LintCode::ReferenceTooLarge,
        LintCode::ContextTooLarge,
        LintCode::DecisionIdInvalid,
        LintCode::DecisionIdDuplicate,
        LintCode::DecisionLimitExceeded,
        LintCode::DecisionOptionsInvalid,
        LintCode::DecisionRecommendationInvalid,
        LintCode::DecisionAnswerMultiple,
        LintCode::DecisionUnanswered,
        LintCode::KindInvalid,
        LintCode::RegressionMissing,
        LintCode::RegressionCommandInvalid,
        LintCode::RegressionTooMany,
        LintCode::RegressionIgnored,
        LintCode::RegressionMultiStep,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            LintCode::NotASpec => "not_a_spec",
            LintCode::FileTooLarge => "file_too_large",
            LintCode::InvalidUtf8 => "invalid_utf8",
            LintCode::SlugInvalid => "slug_invalid",
            LintCode::TitleMissing => "title_missing",
            LintCode::AcSectionMissing => "ac_section_missing",
            LintCode::AcIdInvalid => "ac_id_invalid",
            LintCode::AcIdDuplicate => "ac_id_duplicate",
            LintCode::StepsMissing => "steps_missing",
            LintCode::StepsColumnMissing => "steps_column_missing",
            LintCode::StepIdInvalid => "step_id_invalid",
            LintCode::StepLimitExceeded => "step_limit_exceeded",
            LintCode::StepAcEmpty => "step_ac_empty",
            LintCode::StepAcUnknown => "step_ac_unknown",
            LintCode::StepCommandMissing => "step_command_missing",
            LintCode::StepRepoInvalid => "step_repo_invalid",
            LintCode::StepRepoUnknown => "step_repo_unknown",
            LintCode::FrontmatterInvalid => "frontmatter_invalid",
            LintCode::PriorityInvalid => "priority_invalid",
            LintCode::DependsOnInvalid => "depends_on_invalid",
            LintCode::DependsOnSelf => "depends_on_self",
            LintCode::StepAfterUnknown => "step_after_unknown",
            LintCode::StepAfterCycle => "step_after_cycle",
            LintCode::DoneWhenWithSteps => "done_when_with_steps",
            LintCode::DoneWhenItemInvalid => "done_when_item_invalid",
            LintCode::AcUncovered => "ac_uncovered",
            LintCode::StepWorkEmpty => "step_work_empty",
            LintCode::StepCommandDuplicate => "step_command_duplicate",
            LintCode::StepParallelStart => "step_parallel_start",
            LintCode::DependsOnUnknown => "depends_on_unknown",
            LintCode::DependsOnWithdrawn => "depends_on_withdrawn",
            LintCode::DependsOnCycle => "depends_on_cycle",
            LintCode::LinksLimitExceeded => "links_limit_exceeded",
            LintCode::ReferenceOutsideWorkspace => "reference_outside_workspace",
            LintCode::ReferenceNotDoc => "reference_not_doc",
            LintCode::ReferenceMissing => "reference_missing",
            LintCode::ReferenceNotText => "reference_not_text",
            LintCode::ReferenceTooLarge => "reference_too_large",
            LintCode::ContextTooLarge => "context_too_large",
            LintCode::DecisionIdInvalid => "decision_id_invalid",
            LintCode::DecisionIdDuplicate => "decision_id_duplicate",
            LintCode::DecisionLimitExceeded => "decision_limit_exceeded",
            LintCode::DecisionOptionsInvalid => "decision_options_invalid",
            LintCode::DecisionRecommendationInvalid => "decision_recommendation_invalid",
            LintCode::DecisionAnswerMultiple => "decision_answer_multiple",
            LintCode::DecisionUnanswered => "decision_unanswered",
            LintCode::KindInvalid => "kind_invalid",
            LintCode::RegressionMissing => "regression_missing",
            LintCode::RegressionCommandInvalid => "regression_command_invalid",
            LintCode::RegressionTooMany => "regression_too_many",
            LintCode::RegressionIgnored => "regression_ignored",
            LintCode::RegressionMultiStep => "regression_multi_step",
        }
    }

    pub fn severity(self) -> Severity {
        match self {
            LintCode::AcUncovered
            | LintCode::StepWorkEmpty
            | LintCode::StepCommandDuplicate
            | LintCode::StepParallelStart
            | LintCode::LinksLimitExceeded
            | LintCode::ReferenceNotDoc
            | LintCode::RegressionIgnored
            | LintCode::RegressionMultiStep => Severity::Warning,
            LintCode::NotASpec
            | LintCode::FileTooLarge
            | LintCode::InvalidUtf8
            | LintCode::SlugInvalid
            | LintCode::TitleMissing
            | LintCode::AcSectionMissing
            | LintCode::AcIdInvalid
            | LintCode::AcIdDuplicate
            | LintCode::StepsMissing
            | LintCode::StepsColumnMissing
            | LintCode::StepIdInvalid
            | LintCode::StepLimitExceeded
            | LintCode::StepAcEmpty
            | LintCode::StepAcUnknown
            | LintCode::StepCommandMissing
            | LintCode::StepRepoInvalid
            | LintCode::StepRepoUnknown
            | LintCode::FrontmatterInvalid
            | LintCode::PriorityInvalid
            | LintCode::DependsOnInvalid
            | LintCode::DependsOnSelf
            | LintCode::StepAfterUnknown
            | LintCode::StepAfterCycle
            | LintCode::DoneWhenWithSteps
            | LintCode::DoneWhenItemInvalid
            | LintCode::DependsOnUnknown
            | LintCode::DependsOnWithdrawn
            | LintCode::DependsOnCycle
            | LintCode::ReferenceOutsideWorkspace
            | LintCode::ReferenceMissing
            | LintCode::ReferenceNotText
            | LintCode::ReferenceTooLarge
            | LintCode::ContextTooLarge
            | LintCode::DecisionIdInvalid
            | LintCode::DecisionIdDuplicate
            | LintCode::DecisionLimitExceeded
            | LintCode::DecisionOptionsInvalid
            | LintCode::DecisionRecommendationInvalid
            | LintCode::DecisionAnswerMultiple
            | LintCode::DecisionUnanswered
            | LintCode::KindInvalid
            | LintCode::RegressionMissing
            | LintCode::RegressionCommandInvalid
            | LintCode::RegressionTooMany => Severity::Error,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LintIssue {
    pub code: LintCode,
    #[serde(default)]
    pub severity: Severity,
    pub line: u32,
    pub params: Map<String, Value>,
}

impl LintIssue {
    pub fn new(code: LintCode, line: u32) -> LintIssue {
        LintIssue {
            code,
            severity: code.severity(),
            line,
            params: Map::new(),
        }
    }

    pub fn with(mut self, key: &str, value: impl Into<Value>) -> LintIssue {
        self.params.insert(key.to_string(), value.into());
        self
    }

    pub fn is_error(&self) -> bool {
        self.severity == Severity::Error
    }
}
