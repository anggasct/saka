#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Labels {
    En,
    Id,
}

impl Labels {
    pub fn detect() -> Labels {
        let locale = ["LC_ALL", "LC_MESSAGES", "LANG"]
            .iter()
            .filter_map(|name| std::env::var(name).ok())
            .find(|value| !value.is_empty())
            .unwrap_or_default();
        Labels::from_locale(&locale)
    }

    pub fn from_locale(locale: &str) -> Labels {
        if locale.to_ascii_lowercase().starts_with("id") {
            Labels::Id
        } else {
            Labels::En
        }
    }

    pub fn waiting(self, count: i64) -> String {
        match (self, count) {
            (Labels::En, 1) => "1 item is waiting for you".to_string(),
            (Labels::En, n) => format!("{n} items are waiting for you"),
            (Labels::Id, n) => format!("{n} item menunggumu"),
        }
    }

    pub fn open_app(self) -> &'static str {
        match self {
            Labels::En => "Open Saka",
            Labels::Id => "Buka Saka",
        }
    }

    pub fn open_inbox(self, open: Option<i64>) -> String {
        let base = match self {
            Labels::En => "Open inbox",
            Labels::Id => "Buka inbox",
        };
        match open {
            Some(n) if n > 0 => format!("{base} ({n})"),
            _ => base.to_string(),
        }
    }

    pub fn local_running(self) -> &'static str {
        match self {
            Labels::En => "Local Saka is running",
            Labels::Id => "Saka lokal berjalan",
        }
    }

    pub fn local_starting(self) -> &'static str {
        match self {
            Labels::En => "Local Saka is starting",
            Labels::Id => "Saka lokal sedang mulai",
        }
    }

    pub fn local_stopped(self) -> &'static str {
        match self {
            Labels::En => "Local Saka is stopped",
            Labels::Id => "Saka lokal berhenti",
        }
    }

    pub fn local_lost(self) -> &'static str {
        match self {
            Labels::En => "Disconnected: local Saka stopped unexpectedly",
            Labels::Id => "Terputus: Saka lokal berhenti tiba-tiba",
        }
    }

    pub fn start_local(self) -> &'static str {
        match self {
            Labels::En => "Start local Saka",
            Labels::Id => "Mulai Saka lokal",
        }
    }

    pub fn stop_local(self) -> &'static str {
        match self {
            Labels::En => "Stop local Saka",
            Labels::Id => "Hentikan Saka lokal",
        }
    }

    pub fn update_available(self, version: &str) -> String {
        match self {
            Labels::En => format!("Saka {version} is available"),
            Labels::Id => format!("Saka {version} tersedia"),
        }
    }

    pub fn install_update(self, version: &str) -> String {
        match self {
            Labels::En => format!("Install Saka {version} and restart"),
            Labels::Id => format!("Pasang Saka {version} dan mulai ulang"),
        }
    }

    pub fn quit(self) -> &'static str {
        match self {
            Labels::En => "Quit",
            Labels::Id => "Keluar",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn indonesian_locales_pick_indonesian_and_everything_else_english() {
        for (locale, expected) in [
            ("id_ID.UTF-8", Labels::Id),
            ("ID", Labels::Id),
            ("en_US.UTF-8", Labels::En),
            ("", Labels::En),
            ("C", Labels::En),
        ] {
            assert_eq!(Labels::from_locale(locale), expected, "{locale}");
        }
    }

    #[test]
    fn counts_are_spelled_out_in_both_languages() {
        assert_eq!(Labels::En.waiting(1), "1 item is waiting for you");
        assert_eq!(Labels::En.waiting(3), "3 items are waiting for you");
        assert_eq!(Labels::Id.waiting(3), "3 item menunggumu");
        assert_eq!(Labels::En.open_inbox(Some(2)), "Open inbox (2)");
        assert_eq!(Labels::En.open_inbox(Some(0)), "Open inbox");
        assert_eq!(Labels::Id.open_inbox(None), "Buka inbox");
    }
}
