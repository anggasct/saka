use std::sync::Arc;

use saka_core::secret::SecretText;
use saka_notify::{ChannelKind, ChannelSpec, ChatConfig, Lang, PollTiming};
use saka_ports::Clock;
use serde::Deserialize;

use super::{Config, ConfigError, check_private_file, err, expand, id_ok, url_ok};

pub const MAX_CHANNELS: usize = 8;
pub const MAX_ALLOWED_USERS: usize = 8;
const PUBLIC_URL_MAX: usize = 200;
const TOKEN_MAX_BYTES: usize = 256;
const CHAT_ID_MAX_DIGITS: usize = 20;

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Notify {
    #[serde(default)]
    pub public_url: Option<String>,
    #[serde(default = "english")]
    pub language: String,
    #[serde(default)]
    pub channel: Vec<NotifyChannel>,
}

fn english() -> String {
    "en".into()
}

impl Default for Notify {
    fn default() -> Notify {
        Notify {
            public_url: None,
            language: english(),
            channel: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NotifyChannel {
    pub name: String,
    pub kind: String,
    pub token_file: String,
    pub chat: String,
    #[serde(default)]
    pub api_url: Option<String>,
    #[serde(default)]
    pub actions: bool,
    #[serde(default)]
    pub allowed_users: Vec<String>,
}

fn invalid(code: &str, name: &str, field: Option<&str>) -> ConfigError {
    let field = field.map(|f| format!(".{f}")).unwrap_or_default();
    err(
        "config_notify_invalid",
        format!("{code}: channel {name:?}{field}"),
    )
}

fn digits_ok(raw: &str) -> bool {
    (1..=CHAT_ID_MAX_DIGITS).contains(&raw.len()) && raw.bytes().all(|b| b.is_ascii_digit())
}

fn chat_ok(kind: ChannelKind, raw: &str) -> bool {
    match kind {
        ChannelKind::Telegram => raw.parse::<i64>().is_ok(),
        ChannelKind::Discord => digits_ok(raw),
    }
}

fn public_url_ok(raw: &str) -> bool {
    let Ok(url) = reqwest::Url::parse(raw) else {
        return false;
    };
    raw.len() <= PUBLIC_URL_MAX
        && url_ok(raw)
        && url.username().is_empty()
        && url.password().is_none()
        && url.query().is_none()
        && url.fragment().is_none()
}

impl Notify {
    pub fn channel_kind(channel: &NotifyChannel) -> Option<ChannelKind> {
        ChannelKind::parse(&channel.kind)
    }

    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.channel.iter().map(|c| c.name.as_str())
    }

    pub(super) fn validate(&self) -> Result<(), ConfigError> {
        if let Some(url) = &self.public_url
            && !public_url_ok(url)
        {
            return Err(err("config_notify_invalid", "notify_public_url_invalid"));
        }
        if Lang::parse(&self.language).is_none() {
            return Err(err("config_notify_invalid", "notify_language_invalid"));
        }
        if self.channel.len() > MAX_CHANNELS {
            return Err(err("config_notify_invalid", "notify_limit_exceeded"));
        }
        let mut seen = std::collections::BTreeSet::new();
        for c in &self.channel {
            if !id_ok(&c.name, 32) {
                return Err(invalid(
                    "notify_channel_name_invalid",
                    &c.name,
                    Some("name"),
                ));
            }
            if !seen.insert(c.name.as_str()) {
                return Err(invalid(
                    "notify_channel_name_duplicate",
                    &c.name,
                    Some("name"),
                ));
            }
            let Some(kind) = Notify::channel_kind(c) else {
                return Err(invalid("notify_kind_invalid", &c.name, Some("kind")));
            };
            if c.token_file.trim().is_empty() {
                return Err(invalid("notify_field_invalid", &c.name, Some("token_file")));
            }
            if !chat_ok(kind, &c.chat) {
                return Err(invalid("notify_field_invalid", &c.name, Some("chat")));
            }
            if c.api_url.as_deref().is_some_and(|u| !url_ok(u)) {
                return Err(invalid("notify_field_invalid", &c.name, Some("api_url")));
            }
            c.validate_actions(kind)?;
        }
        Ok(())
    }
}

impl NotifyChannel {
    fn validate_actions(&self, kind: ChannelKind) -> Result<(), ConfigError> {
        let name = self.name.as_str();
        if self.actions && kind != ChannelKind::Telegram {
            return Err(invalid("notify_actions_unsupported", name, Some("actions")));
        }
        if self.actions && self.allowed_users.is_empty() {
            return Err(invalid(
                "notify_allowed_users_required",
                name,
                Some("allowed_users"),
            ));
        }
        let unique: std::collections::BTreeSet<&str> =
            self.allowed_users.iter().map(String::as_str).collect();
        let listed_without_actions = !self.actions && !self.allowed_users.is_empty();
        if listed_without_actions
            || self.allowed_users.len() > MAX_ALLOWED_USERS
            || unique.len() != self.allowed_users.len()
            || !self.allowed_users.iter().all(|u| digits_ok(u))
        {
            return Err(invalid("notify_field_invalid", name, Some("allowed_users")));
        }
        Ok(())
    }
}

fn read_token(channel: &NotifyChannel) -> Result<SecretText, ConfigError> {
    let path = expand(&channel.token_file);
    check_private_file(&path)?;
    let mut bytes = std::fs::read(&path).map_err(|e| {
        err(
            "config_key_file_unreadable",
            format!("{}: {e}", path.display()),
        )
    })?;
    while bytes.last().is_some_and(|b| *b == b'\n' || *b == b'\r') {
        bytes.pop();
    }
    let token = String::from_utf8(bytes).ok().filter(|t| {
        (1..=TOKEN_MAX_BYTES).contains(&t.len())
            && t.chars().all(|c| !c.is_whitespace() && !c.is_control())
    });
    token
        .map(SecretText::new)
        .ok_or_else(|| err("config_notify_token_invalid", path.display().to_string()))
}

impl Config {
    pub fn notify_token_files(&self) -> impl Iterator<Item = &str> {
        self.notify.channel.iter().map(|c| c.token_file.as_str())
    }

    pub fn chat_config(&self, user_agent: &str) -> Result<ChatConfig, ConfigError> {
        let mut channels = Vec::new();
        for c in &self.notify.channel {
            let Some(kind) = Notify::channel_kind(c) else {
                return Err(invalid("notify_kind_invalid", &c.name, Some("kind")));
            };
            channels.push(ChannelSpec {
                name: c.name.clone(),
                kind,
                chat: c.chat.clone(),
                api_url: c
                    .api_url
                    .clone()
                    .unwrap_or_else(|| kind.default_api_url().to_string()),
                token: read_token(c)?,
                allowed_users: c.actions.then(|| c.allowed_users.clone()),
            });
        }
        Ok(ChatConfig {
            language: Lang::parse(&self.notify.language).unwrap_or(Lang::En),
            public_url: self.notify.public_url.clone(),
            channels,
            user_agent: user_agent.to_string(),
            poll: PollTiming::default(),
        })
    }

    pub fn chat_notifier(
        &self,
        user_agent: &str,
        clock: &Arc<dyn Clock>,
    ) -> Result<saka_notify::ChatNotifier, ConfigError> {
        saka_notify::ChatNotifier::new(self.chat_config(user_agent)?, clock)
            .map_err(|e| err("config_notify_invalid", e.code))
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use super::*;

    fn channel(extra: &str) -> String {
        format!(
            "[[notify.channel]]\nname = \"ops\"\nkind = \"telegram\"\ntoken_file = \"/t\"\nchat = \"-100\"\n{extra}"
        )
    }

    fn refused(text: &str) -> String {
        let e = Config::parse(text).unwrap_err();
        assert_eq!(e.code, "config_notify_invalid", "{text}");
        e.detail
    }

    #[test]
    fn without_a_notify_table_nothing_is_configured() {
        let cfg = Config::parse("").unwrap();
        assert!(cfg.notify.channel.is_empty());
        assert_eq!(cfg.notify.language, "en");
        assert_eq!(cfg.notify.public_url, None);
    }

    #[test]
    fn a_valid_channel_parses_and_the_tables_reject_unknown_fields() {
        let cfg = Config::parse(&format!(
            "[notify]\npublic_url = \"https://saka.example.com\"\nlanguage = \"id\"\n{}{}",
            channel(""),
            "[[notify.channel]]\nname = \"chat\"\nkind = \"discord\"\ntoken_file = \"/d\"\nchat = \"998877\"\napi_url = \"http://127.0.0.1:9\"\n"
        ))
        .unwrap();
        assert_eq!(cfg.notify.channel.len(), 2);
        assert_eq!(cfg.notify.language, "id");
        assert_eq!(
            Config::parse(&channel("nope = 1\n")).unwrap_err().code,
            "config_unknown_field"
        );
        assert_eq!(
            Config::parse("[notify]\nnope = 1\n").unwrap_err().code,
            "config_unknown_field"
        );
    }

    #[test]
    fn each_channel_rule_has_a_fixed_code_naming_the_channel_and_field() {
        let swap = |from: &str, to: &str| channel("").replace(from, to);
        for (text, code) in [
            (
                swap("name = \"ops\"", "name = \"Ops\""),
                "notify_channel_name_invalid",
            ),
            (
                format!("{}{}", channel(""), channel("")),
                "notify_channel_name_duplicate",
            ),
            (swap("telegram", "slack"), "notify_kind_invalid"),
            (
                swap("\"-100\"", "\"abc\""),
                "notify_field_invalid: channel \"ops\".chat",
            ),
            (
                swap("/t", " "),
                "notify_field_invalid: channel \"ops\".token_file",
            ),
            (
                channel("api_url = \"http://example.com\"\n"),
                "notify_field_invalid: channel \"ops\".api_url",
            ),
            (
                swap("telegram", "discord").replace("-100", "-1"),
                "notify_field_invalid: channel \"ops\".chat",
            ),
        ] {
            assert!(refused(&text).starts_with(code), "{text}");
        }
        let many: String = (0..=MAX_CHANNELS)
            .map(|i| channel("").replace("ops", &format!("c{i}")))
            .collect();
        assert_eq!(refused(&many), "notify_limit_exceeded");
    }

    #[test]
    fn actions_need_a_telegram_channel_and_a_list_of_users_who_may_press_the_buttons() {
        let on = |extra: &str| channel(&format!("actions = true\n{extra}"));
        let cfg = Config::parse(&on("allowed_users = [\"4242\", \"77\"]\n")).unwrap();
        assert!(cfg.notify.channel[0].actions);
        for (text, code) in [
            (on(""), "notify_allowed_users_required"),
            (
                on("allowed_users = [\"1\"]\n")
                    .replace("telegram", "discord")
                    .replace("-100", "100"),
                "notify_actions_unsupported",
            ),
            (
                on("allowed_users = [\"x1\"]\n"),
                "notify_field_invalid: channel \"ops\".allowed_users",
            ),
            (
                on("allowed_users = [\"-5\"]\n"),
                "notify_field_invalid: channel \"ops\".allowed_users",
            ),
            (
                on("allowed_users = [\"1\", \"1\"]\n"),
                "notify_field_invalid: channel \"ops\".allowed_users",
            ),
            (
                on("allowed_users = [\"1\",\"2\",\"3\",\"4\",\"5\",\"6\",\"7\",\"8\",\"9\"]\n"),
                "notify_field_invalid: channel \"ops\".allowed_users",
            ),
            (
                channel("allowed_users = [\"1\"]\n"),
                "notify_field_invalid: channel \"ops\".allowed_users",
            ),
        ] {
            assert!(refused(&text).starts_with(code), "{text}");
        }
    }

    #[test]
    fn the_public_url_and_language_are_checked() {
        for url in [
            "http://saka.example.com",
            "https://user:pw@saka.example.com",
            "https://saka.example.com/?x=1",
            "https://saka.example.com/#top",
            "ftp://saka.example.com",
            "saka.example.com",
        ] {
            assert_eq!(
                refused(&format!("[notify]\npublic_url = {url:?}\n")),
                "notify_public_url_invalid",
                "{url}"
            );
        }
        let long = format!("https://{}.example.com", "a".repeat(200));
        assert_eq!(
            refused(&format!("[notify]\npublic_url = {long:?}\n")),
            "notify_public_url_invalid"
        );
        for ok in [
            "https://saka.example.com",
            "http://127.0.0.1:7420/saka",
            "http://localhost",
        ] {
            Config::parse(&format!("[notify]\npublic_url = {ok:?}\n")).unwrap();
        }
        assert_eq!(
            refused("[notify]\nlanguage = \"fr\"\n"),
            "notify_language_invalid"
        );
    }

    #[test]
    fn a_notify_hook_must_name_a_configured_channel() {
        let hook = "[[hook]]\nname = \"tell\"\nproject = \"tasks\"\non = \"merged\"\naction = \"notify\"\nchannel = \"ops\"\n";
        assert!(refused(hook).starts_with("notify_hook_channel_unknown"));
        Config::parse(&format!("{}{hook}", channel(""))).unwrap();
    }

    #[test]
    fn the_token_file_must_be_private_and_hold_one_token() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("ops.token");
        let cfg = Config::parse(&channel("").replace("/t", &file.display().to_string())).unwrap();
        assert_eq!(
            cfg.check_key_files().unwrap_err().code,
            "config_key_file_unreadable"
        );
        std::fs::write(&file, "123:token\n").unwrap();
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o640)).unwrap();
        assert_eq!(
            cfg.check_key_files().unwrap_err().code,
            "config_key_file_mode"
        );
        assert_eq!(
            cfg.chat_config("saka/test").err().map(|e| e.code),
            Some("config_key_file_mode")
        );
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600)).unwrap();
        cfg.check_key_files().unwrap();
        let chat = cfg.chat_config("saka/test").unwrap();
        assert_eq!(chat.channels[0].token.expose(), "123:token");
        assert_eq!(chat.channels[0].api_url, "https://api.telegram.org");
        assert_eq!(chat.channels[0].allowed_users, None);
        let with_actions = Config::parse(
            &channel("actions = true\nallowed_users = [\"42\"]\n")
                .replace("/t", &file.display().to_string()),
        )
        .unwrap();
        assert_eq!(
            with_actions.chat_config("saka/test").unwrap().channels[0].allowed_users,
            Some(vec!["42".to_string()])
        );
        for bad in ["", "two words", "a\u{7}b", &"x".repeat(300)] {
            std::fs::write(&file, bad).unwrap();
            assert_eq!(
                cfg.chat_config("saka/test").err().map(|e| e.code),
                Some("config_notify_token_invalid"),
                "{bad:?}"
            );
        }
    }
}
