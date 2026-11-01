use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

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
}

impl LintCode {
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
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LintIssue {
    pub code: LintCode,
    pub line: u32,
    pub params: Map<String, Value>,
}

impl LintIssue {
    pub(crate) fn new(code: LintCode, line: u32) -> LintIssue {
        LintIssue {
            code,
            line,
            params: Map::new(),
        }
    }

    pub(crate) fn with(mut self, key: &str, value: impl Into<Value>) -> LintIssue {
        self.params.insert(key.to_string(), value.into());
        self
    }
}
