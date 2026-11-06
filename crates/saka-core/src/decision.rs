use serde::{Deserialize, Serialize};

pub const QUESTION_MAX: usize = 500;
pub const LABEL_MAX: usize = 100;
pub const DETAIL_MAX: usize = 500;
pub const CONTEXT_MAX: usize = 2000;
pub const OPTIONS_MIN: usize = 2;
pub const OPTIONS_MAX: usize = 3;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DecisionOption {
    pub label: String,
    #[serde(default)]
    pub detail: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Question {
    pub question: String,
    pub options: Vec<DecisionOption>,
    #[serde(default)]
    pub context: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("invalid question field {field}")]
pub struct InvalidQuestion {
    pub field: &'static str,
}

fn within(text: &str, max: usize) -> bool {
    let chars = text.chars().count();
    chars >= 1 && chars <= max && !text.trim().is_empty()
}

impl Question {
    pub fn validate(&self) -> Result<(), InvalidQuestion> {
        let bad = |field| Err(InvalidQuestion { field });
        if !within(&self.question, QUESTION_MAX) {
            return bad("question");
        }
        if !(OPTIONS_MIN..=OPTIONS_MAX).contains(&self.options.len()) {
            return bad("options");
        }
        for option in &self.options {
            if !within(&option.label, LABEL_MAX) {
                return bad("options.label");
            }
            if option
                .detail
                .as_deref()
                .is_some_and(|d| d.chars().count() > DETAIL_MAX)
            {
                return bad("options.detail");
            }
        }
        if self
            .context
            .as_deref()
            .is_some_and(|c| c.chars().count() > CONTEXT_MAX)
        {
            return bad("context");
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn option(label: &str) -> DecisionOption {
        DecisionOption {
            label: label.into(),
            detail: None,
        }
    }

    fn question() -> Question {
        Question {
            question: "Where do we store audio?".into(),
            options: vec![option("Local"), option("R2")],
            context: None,
        }
    }

    #[test]
    fn a_question_with_two_options_is_valid() {
        assert_eq!(question().validate(), Ok(()));
    }

    #[test]
    fn each_field_has_its_own_limit() {
        let mut q = question();
        q.question = String::new();
        assert_eq!(q.validate(), Err(InvalidQuestion { field: "question" }));
        let mut q = question();
        q.question = "x".repeat(QUESTION_MAX + 1);
        assert_eq!(q.validate(), Err(InvalidQuestion { field: "question" }));
        let mut q = question();
        q.options.pop();
        assert_eq!(q.validate(), Err(InvalidQuestion { field: "options" }));
        let mut q = question();
        q.options = vec![option("a"), option("b"), option("c"), option("d")];
        assert_eq!(q.validate(), Err(InvalidQuestion { field: "options" }));
        let mut q = question();
        q.options[1].label = "  ".into();
        assert_eq!(
            q.validate(),
            Err(InvalidQuestion {
                field: "options.label"
            })
        );
        let mut q = question();
        q.options[0].detail = Some("x".repeat(DETAIL_MAX + 1));
        assert_eq!(
            q.validate(),
            Err(InvalidQuestion {
                field: "options.detail"
            })
        );
        let mut q = question();
        q.context = Some("x".repeat(CONTEXT_MAX + 1));
        assert_eq!(q.validate(), Err(InvalidQuestion { field: "context" }));
    }

    #[test]
    fn limits_count_characters_not_bytes() {
        let mut q = question();
        q.question = "é".repeat(QUESTION_MAX);
        assert_eq!(q.validate(), Ok(()));
    }
}
