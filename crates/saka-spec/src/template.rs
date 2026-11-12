use saka_core::vocab::WorkKind;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Locale {
    #[default]
    En,
    Id,
}

impl Locale {
    pub const ALL: [Locale; 2] = [Locale::En, Locale::Id];

    pub fn as_str(self) -> &'static str {
        match self {
            Locale::En => "en",
            Locale::Id => "id",
        }
    }

    pub fn from_code(code: &str) -> Option<Locale> {
        Locale::ALL.into_iter().find(|l| l.as_str() == code)
    }
}

pub fn template(locale: Locale, kind: WorkKind) -> &'static str {
    match (locale, kind) {
        (Locale::En, WorkKind::Feature) => include_str!("../templates/en/feature.md"),
        (Locale::En, WorkKind::Bugfix) => include_str!("../templates/en/bugfix.md"),
        (Locale::En, WorkKind::Refactor) => include_str!("../templates/en/refactor.md"),
        (Locale::En, WorkKind::Chore) => include_str!("../templates/en/chore.md"),
        (Locale::En, WorkKind::SecurityPatch) => include_str!("../templates/en/security_patch.md"),
        (Locale::Id, WorkKind::Feature) => include_str!("../templates/id/feature.md"),
        (Locale::Id, WorkKind::Bugfix) => include_str!("../templates/id/bugfix.md"),
        (Locale::Id, WorkKind::Refactor) => include_str!("../templates/id/refactor.md"),
        (Locale::Id, WorkKind::Chore) => include_str!("../templates/id/chore.md"),
        (Locale::Id, WorkKind::SecurityPatch) => include_str!("../templates/id/security_patch.md"),
    }
}
