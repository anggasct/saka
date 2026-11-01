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

pub fn template(locale: Locale) -> &'static str {
    match locale {
        Locale::En => include_str!("../templates/en.md"),
        Locale::Id => include_str!("../templates/id.md"),
    }
}
