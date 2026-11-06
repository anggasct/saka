use crate::labels::Labels;
use crate::sidecar::Phase;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Remote,
    Local(Phase),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub id: &'static str,
    pub label: String,
    pub enabled: bool,
}

pub const OPEN: &str = "open";
pub const INBOX: &str = "inbox";
pub const START: &str = "start";
pub const STOP: &str = "stop";
pub const QUIT: &str = "quit";
pub const STATUS: &str = "status";

fn entry(id: &'static str, label: impl Into<String>, enabled: bool) -> Entry {
    Entry {
        id,
        label: label.into(),
        enabled,
    }
}

pub fn menu(mode: Mode, open: Option<i64>, labels: Labels) -> Vec<Entry> {
    let mut entries = Vec::new();
    if let Mode::Local(phase) = mode {
        let status = match phase {
            Phase::Running => labels.local_running(),
            Phase::Starting => labels.local_starting(),
            Phase::Stopped => labels.local_stopped(),
            Phase::Exited => labels.local_lost(),
        };
        entries.push(entry(STATUS, status, false));
    }
    entries.push(entry(OPEN, labels.open_app(), true));
    entries.push(entry(INBOX, labels.open_inbox(open), true));
    if let Mode::Local(phase) = mode {
        let running = matches!(phase, Phase::Running | Phase::Starting);
        entries.push(entry(START, labels.start_local(), !running));
        entries.push(entry(STOP, labels.stop_local(), running));
    }
    entries.push(entry(QUIT, labels.quit(), true));
    entries
}

pub fn tooltip(open: Option<i64>, labels: Labels) -> String {
    match open {
        Some(n) if n > 0 => format!("Saka: {}", labels.waiting(n)),
        _ => "Saka".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids(entries: &[Entry]) -> Vec<&'static str> {
        entries.iter().map(|e| e.id).collect()
    }

    fn enabled(entries: &[Entry], id: &str) -> bool {
        entries.iter().find(|e| e.id == id).unwrap().enabled
    }

    #[test]
    fn a_remote_connection_has_no_local_controls() {
        let entries = menu(Mode::Remote, Some(3), Labels::En);
        assert_eq!(ids(&entries), [OPEN, INBOX, QUIT]);
        assert_eq!(entries[1].label, "Open inbox (3)");
    }

    #[test]
    fn a_running_local_saka_can_be_stopped_but_not_started() {
        let entries = menu(Mode::Local(Phase::Running), None, Labels::En);
        assert_eq!(ids(&entries), [STATUS, OPEN, INBOX, START, STOP, QUIT]);
        assert!(!enabled(&entries, START));
        assert!(enabled(&entries, STOP));
        assert_eq!(entries[0].label, "Local Saka is running");
        assert!(!enabled(&entries, STATUS));
    }

    #[test]
    fn a_local_saka_that_died_shows_disconnected_and_offers_to_start_again() {
        let entries = menu(Mode::Local(Phase::Exited), None, Labels::En);
        assert_eq!(
            entries[0].label,
            "Disconnected: local Saka stopped unexpectedly"
        );
        assert!(enabled(&entries, START));
        assert!(!enabled(&entries, STOP));
        let stopped = menu(Mode::Local(Phase::Stopped), None, Labels::Id);
        assert!(enabled(&stopped, START));
    }

    #[test]
    fn the_tooltip_names_what_waits() {
        assert_eq!(
            tooltip(Some(2), Labels::En),
            "Saka: 2 items are waiting for you"
        );
        assert_eq!(tooltip(Some(0), Labels::En), "Saka");
        assert_eq!(tooltip(None, Labels::Id), "Saka");
    }
}
