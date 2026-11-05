use saka_core::ids::BlockerId;
use saka_ports::{ChatActionKind, ChatOutcome, Notification};
use serde_json::Value;
use uuid::Uuid;

pub const TEXT_MAX_CHARS: usize = 1500;
const NAME_MAX_CHARS: usize = 80;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lang {
    En,
    Id,
}

impl Lang {
    pub fn parse(raw: &str) -> Option<Lang> {
        match raw {
            "en" => Some(Lang::En),
            "id" => Some(Lang::Id),
            _ => None,
        }
    }

    fn pick<'a>(self, en: &'a str, id: &'a str) -> &'a str {
        match self {
            Lang::En => en,
            Lang::Id => id,
        }
    }
}

fn clean(value: &str, max: usize) -> String {
    let mut out: String = value.chars().filter(|c| !c.is_control()).collect();
    if out.chars().count() > max {
        out = out.chars().take(max.saturating_sub(1)).collect();
        out.push('…');
    }
    out
}

fn text_at<'a>(params: &'a Value, path: &[&str]) -> Option<&'a str> {
    let mut at = params;
    for key in path {
        at = at.get(key)?;
    }
    at.as_str().filter(|s| !s.is_empty())
}

fn link(public_url: Option<&str>, params: &Value) -> Option<String> {
    let base = public_url?.trim_end_matches('/');
    Some(match text_at(params, &["task", "id"]) {
        Some(id) => format!("{base}/delivery/tasks/{}", clean(id, 64)),
        None => format!("{base}/delivery"),
    })
}

fn subject(params: &Value) -> String {
    let named = text_at(params, &["task", "key"])
        .or_else(|| text_at(params, &["spec", "path"]))
        .map(str::to_string);
    let numbered = || {
        params
            .pointer("/change/number")
            .and_then(Value::as_u64)
            .map(|n| format!("#{n}"))
    };
    clean(
        &named.or_else(numbered).unwrap_or_else(|| "-".into()),
        NAME_MAX_CHARS,
    )
}

fn headline(lang: Lang, event: &str, subject: &str, project: &str) -> String {
    let (en, id) = match event {
        "blocked" => ("Blocked", "Terhenti"),
        "merged" => ("Merged", "Di-merge"),
        "task_done" => ("Done", "Selesai"),
        "change_opened" => ("Pull request opened", "Pull request dibuka"),
        "spec_shipped" => ("Spec shipped", "Spec selesai"),
        _ => ("Saka event", "Event Saka"),
    };
    let word = lang.pick(en, id);
    let preposition = lang.pick("in", "di");
    format!("{word}: {subject} {preposition} {project}")
}

pub fn render(lang: Lang, notification: &Notification, public_url: Option<&str>) -> String {
    let params = &notification.params;
    let mut lines = Vec::new();
    match notification.code.as_str() {
        "hook.event" => {
            let event = text_at(params, &["event"]).unwrap_or("");
            let project = clean(
                text_at(params, &["project", "name"]).unwrap_or("-"),
                NAME_MAX_CHARS,
            );
            lines.push(headline(lang, event, &subject(params), &project));
            if event == "blocked"
                && let Some(code) = text_at(params, &["blocker", "code"])
            {
                lines.push(format!(
                    "{}: {}",
                    lang.pick("Reason", "Penyebab"),
                    clean(code, 64)
                ));
            }
            lines.extend(link(public_url, params));
        }
        "notify.test" => lines.push(
            lang.pick("Saka: test message", "Saka: pesan uji")
                .to_string(),
        ),
        other => lines.push(format!("Saka: {}", clean(other, 64))),
    }
    lines.join("\n").chars().take(TEXT_MAX_CHARS).collect()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Button {
    pub kind: ChatActionKind,
    pub blocker_id: BlockerId,
    pub label: String,
}

impl Button {
    pub fn data(&self) -> String {
        callback_data(self.kind, self.blocker_id)
    }
}

pub fn callback_data(kind: ChatActionKind, blocker_id: BlockerId) -> String {
    let tag = match kind {
        ChatActionKind::Retry => 'r',
        ChatActionKind::Cancel => 'c',
    };
    format!("{tag}:{}", blocker_id.0.simple())
}

pub fn parse_callback(data: &str) -> Option<(ChatActionKind, BlockerId)> {
    let (tag, id) = data.split_once(':')?;
    let kind = match tag {
        "r" => ChatActionKind::Retry,
        "c" => ChatActionKind::Cancel,
        _ => return None,
    };
    (id.len() == 32).then_some(())?;
    Uuid::parse_str(id).ok().map(|id| (kind, BlockerId(id)))
}

pub fn button(lang: Lang, notification: &Notification) -> Option<Button> {
    let params = &notification.params;
    if notification.code != "hook.event" || text_at(params, &["event"]) != Some("blocked") {
        return None;
    }
    let (kind, label) = match text_at(params, &["blocker", "action"])? {
        "retry" => (ChatActionKind::Retry, lang.pick("Retry", "Coba lagi")),
        "cancel" => (
            ChatActionKind::Cancel,
            lang.pick("Cancel task", "Batalkan task"),
        ),
        _ => return None,
    };
    let id = Uuid::parse_str(text_at(params, &["blocker", "id"])?).ok()?;
    Some(Button {
        kind,
        blocker_id: BlockerId(id),
        label: label.to_string(),
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reply {
    Retried,
    Cancelled,
    Gone,
    Changed,
    NeedsInput,
    Failed,
    NotAllowed,
    Invalid,
}

impl Reply {
    pub fn of(outcome: ChatOutcome, kind: ChatActionKind) -> Reply {
        match (outcome, kind) {
            (ChatOutcome::Done, ChatActionKind::Retry) => Reply::Retried,
            (ChatOutcome::Done, ChatActionKind::Cancel) => Reply::Cancelled,
            (ChatOutcome::Gone, _) => Reply::Gone,
            (ChatOutcome::Changed, _) => Reply::Changed,
            (ChatOutcome::NeedsInput, _) => Reply::NeedsInput,
            (ChatOutcome::Failed, _) => Reply::Failed,
        }
    }

    pub fn settles(self) -> bool {
        matches!(
            self,
            Reply::Retried | Reply::Cancelled | Reply::Gone | Reply::Changed | Reply::NeedsInput
        )
    }

    pub fn text(self, lang: Lang) -> &'static str {
        let (en, id) = match self {
            Reply::Retried => ("Retry started.", "Coba lagi dimulai."),
            Reply::Cancelled => ("Task cancelled.", "Task dibatalkan."),
            Reply::Gone => ("No longer pending.", "Sudah tidak berlaku."),
            Reply::Changed => ("The action changed. Open Saka.", "Aksi berubah. Buka Saka."),
            Reply::NeedsInput => ("Needs input. Open Saka.", "Perlu masukan. Buka Saka."),
            Reply::Failed => ("Could not do that. Try again.", "Gagal. Coba lagi."),
            Reply::NotAllowed => ("Not allowed.", "Tidak diizinkan."),
            Reply::Invalid => ("Invalid button.", "Tombol tidak valid."),
        };
        lang.pick(en, id)
    }
}
