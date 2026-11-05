use saka_ports::Notification;
use serde_json::Value;

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
