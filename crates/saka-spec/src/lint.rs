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
    StepsColumnUnsupported,
    StepIdInvalid,
    StepLimitExceeded,
    StepAcEmpty,
    StepAcUnknown,
    StepCommandMissing,
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
}

impl LintCode {
    pub const ALL: [LintCode; 31] = [
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
        LintCode::StepsColumnUnsupported,
        LintCode::StepIdInvalid,
        LintCode::StepLimitExceeded,
        LintCode::StepAcEmpty,
        LintCode::StepAcUnknown,
        LintCode::StepCommandMissing,
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
            LintCode::StepsColumnUnsupported => "steps_column_unsupported",
            LintCode::StepIdInvalid => "step_id_invalid",
            LintCode::StepLimitExceeded => "step_limit_exceeded",
            LintCode::StepAcEmpty => "step_ac_empty",
            LintCode::StepAcUnknown => "step_ac_unknown",
            LintCode::StepCommandMissing => "step_command_missing",
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
        }
    }

    pub fn severity(self) -> Severity {
        match self {
            LintCode::AcUncovered
            | LintCode::StepWorkEmpty
            | LintCode::StepCommandDuplicate
            | LintCode::StepParallelStart => Severity::Warning,
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
            | LintCode::StepsColumnUnsupported
            | LintCode::StepIdInvalid
            | LintCode::StepLimitExceeded
            | LintCode::StepAcEmpty
            | LintCode::StepAcUnknown
            | LintCode::StepCommandMissing
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
            | LintCode::DependsOnCycle => Severity::Error,
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
