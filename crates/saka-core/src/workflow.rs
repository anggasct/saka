use serde::{Deserialize, Serialize};

use crate::scope::PathScope;
use crate::vocab::{Stage, Trigger, WorkflowPreset};

pub const MAX_FILTER_ENTRIES: usize = 16;
pub const MAX_FILTER_BYTES: usize = 100;
pub const DEFAULT_TRIGGER_LABEL: &str = "saka:review";
pub const DEFAULT_FIX_ROUNDS: u32 = 2;
pub const MAX_FIX_ROUNDS: u32 = 10;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct StageSetting {
    pub stage: Stage,
    pub trigger: Trigger,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChangeEntry {
    pub trigger: Trigger,
    pub labels: Vec<String>,
    pub authors: Vec<String>,
    pub branches: Vec<String>,
    pub paths: Vec<String>,
    pub include_forks: bool,
    pub include_drafts: bool,
    pub trigger_label: String,
}

impl Default for ChangeEntry {
    fn default() -> ChangeEntry {
        ChangeEntry {
            trigger: Trigger::Auto,
            labels: Vec::new(),
            authors: Vec::new(),
            branches: Vec::new(),
            paths: Vec::new(),
            include_forks: false,
            include_drafts: false,
            trigger_label: DEFAULT_TRIGGER_LABEL.to_string(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Workflow {
    pub preset: WorkflowPreset,
    pub spec: bool,
    pub change_request: Option<ChangeEntry>,
    pub stages: Vec<StageSetting>,
    #[serde(default = "default_fix_rounds")]
    pub fix_rounds: u32,
}

fn default_fix_rounds() -> u32 {
    DEFAULT_FIX_ROUNDS
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkflowIssue {
    NoEntry,
    DevWithoutSpec,
    SpecWithoutDev,
    FixWithoutReview,
    ChangeRequestNeedsReview,
}

impl WorkflowIssue {
    pub fn code(self) -> &'static str {
        match self {
            WorkflowIssue::NoEntry => "workflow_no_entry",
            WorkflowIssue::DevWithoutSpec => "workflow_dev_without_spec",
            WorkflowIssue::SpecWithoutDev => "workflow_spec_without_dev",
            WorkflowIssue::FixWithoutReview => "workflow_fix_without_review",
            WorkflowIssue::ChangeRequestNeedsReview => "workflow_change_request_needs_review",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Candidate<'a> {
    pub labels: &'a [String],
    pub author: &'a str,
    pub head_branch: &'a str,
    pub from_fork: bool,
    pub draft: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Skip {
    Fork,
    Draft,
    Label,
    Author,
    Branch,
}

impl Skip {
    pub fn as_str(self) -> &'static str {
        match self {
            Skip::Fork => "fork",
            Skip::Draft => "draft",
            Skip::Label | Skip::Author | Skip::Branch => "filter",
        }
    }
}

fn stage(stage: Stage, trigger: Trigger) -> StageSetting {
    StageSetting { stage, trigger }
}

impl Default for Workflow {
    fn default() -> Workflow {
        Workflow::of(WorkflowPreset::FullLoop)
    }
}

impl Workflow {
    pub fn of(preset: WorkflowPreset) -> Workflow {
        match preset {
            WorkflowPreset::FullLoop => Workflow {
                preset,
                spec: true,
                change_request: None,
                fix_rounds: DEFAULT_FIX_ROUNDS,
                stages: vec![
                    stage(Stage::Dev, Trigger::Auto),
                    stage(Stage::Review, Trigger::Auto),
                    stage(Stage::Fix, Trigger::Auto),
                    stage(Stage::Merge, Trigger::Auto),
                ],
            },
            WorkflowPreset::BuildOnly => Workflow {
                preset,
                spec: true,
                change_request: None,
                fix_rounds: DEFAULT_FIX_ROUNDS,
                stages: vec![stage(Stage::Dev, Trigger::Auto)],
            },
            WorkflowPreset::ReviewOnly => Workflow {
                preset,
                spec: false,
                change_request: Some(ChangeEntry::default()),
                fix_rounds: DEFAULT_FIX_ROUNDS,
                stages: vec![
                    stage(Stage::Review, Trigger::Auto),
                    stage(Stage::Merge, Trigger::Manual),
                ],
            },
            WorkflowPreset::Custom => Workflow {
                preset,
                spec: false,
                change_request: None,
                fix_rounds: DEFAULT_FIX_ROUNDS,
                stages: Vec::new(),
            },
        }
    }

    pub fn stage_trigger(&self, stage: Stage) -> Option<Trigger> {
        self.stages
            .iter()
            .find(|s| s.stage == stage)
            .map(|s| s.trigger)
    }

    pub fn has_stage(&self, stage: Stage) -> bool {
        self.stage_trigger(stage).is_some()
    }

    pub fn remove_stage(&mut self, stage: Stage) {
        self.stages.retain(|s| s.stage != stage);
    }

    pub fn set_stage(&mut self, stage: Stage, trigger: Trigger) {
        match self.stages.iter_mut().find(|s| s.stage == stage) {
            Some(existing) => existing.trigger = trigger,
            None => {
                self.stages.push(StageSetting { stage, trigger });
                self.stages.sort_by_key(|s| s.stage);
            }
        }
    }

    pub fn validate(&self) -> Vec<WorkflowIssue> {
        let mut issues = Vec::new();
        if !self.spec && self.change_request.is_none() {
            issues.push(WorkflowIssue::NoEntry);
        }
        let dev = self.has_stage(Stage::Dev);
        if dev && !self.spec {
            issues.push(WorkflowIssue::DevWithoutSpec);
        }
        if self.spec && !dev {
            issues.push(WorkflowIssue::SpecWithoutDev);
        }
        let review = self.has_stage(Stage::Review);
        if self.has_stage(Stage::Fix) && !review {
            issues.push(WorkflowIssue::FixWithoutReview);
        }
        if self.change_request.is_some() && !review {
            issues.push(WorkflowIssue::ChangeRequestNeedsReview);
        }
        issues
    }
}

fn contains_ignore_case(list: &[String], value: &str) -> bool {
    list.iter().any(|v| v.eq_ignore_ascii_case(value))
}

pub fn filter_text_ok(text: &str) -> bool {
    (1..=MAX_FILTER_BYTES).contains(&text.len()) && !text.chars().any(char::is_control)
}

impl ChangeEntry {
    pub fn screen(&self, candidate: &Candidate<'_>) -> Result<(), Skip> {
        if candidate.from_fork && !self.include_forks {
            return Err(Skip::Fork);
        }
        if candidate.draft && !self.include_drafts {
            return Err(Skip::Draft);
        }
        if !self.labels.is_empty()
            && !candidate
                .labels
                .iter()
                .any(|l| contains_ignore_case(&self.labels, l))
        {
            return Err(Skip::Label);
        }
        if !self.authors.is_empty() && !contains_ignore_case(&self.authors, candidate.author) {
            return Err(Skip::Author);
        }
        if !self.branches.is_empty() {
            let matched = PathScope::parse(&self.branches)
                .is_ok_and(|scope| scope.contains(candidate.head_branch));
            if !matched {
                return Err(Skip::Branch);
            }
        }
        Ok(())
    }

    pub fn needs_files(&self) -> bool {
        !self.paths.is_empty()
    }

    pub fn touches(&self, files: &[String]) -> bool {
        PathScope::parse(&self.paths).is_ok_and(|scope| files.iter().any(|f| scope.contains(f)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry() -> ChangeEntry {
        ChangeEntry::default()
    }

    fn candidate<'a>(labels: &'a [String]) -> Candidate<'a> {
        Candidate {
            labels,
            author: "Alice",
            head_branch: "feature/retries",
            from_fork: false,
            draft: false,
        }
    }

    #[test]
    fn the_presets_expand_to_the_documented_structure() {
        let full = Workflow::of(WorkflowPreset::FullLoop);
        assert!(full.spec && full.change_request.is_none());
        assert_eq!(
            full.stages
                .iter()
                .map(|s| (s.stage, s.trigger))
                .collect::<Vec<_>>(),
            [
                (Stage::Dev, Trigger::Auto),
                (Stage::Review, Trigger::Auto),
                (Stage::Fix, Trigger::Auto),
                (Stage::Merge, Trigger::Auto),
            ]
        );
        let build = Workflow::of(WorkflowPreset::BuildOnly);
        assert!(build.spec && build.stages == [stage(Stage::Dev, Trigger::Auto)]);
        let review = Workflow::of(WorkflowPreset::ReviewOnly);
        assert!(!review.spec);
        assert_eq!(review.change_request, Some(ChangeEntry::default()));
        assert_eq!(
            review.stages,
            [
                stage(Stage::Review, Trigger::Auto),
                stage(Stage::Merge, Trigger::Manual)
            ]
        );
        for preset in [
            WorkflowPreset::FullLoop,
            WorkflowPreset::BuildOnly,
            WorkflowPreset::ReviewOnly,
        ] {
            assert_eq!(Workflow::of(preset).validate(), []);
        }
    }

    #[test]
    fn every_meaningless_combination_reports_its_own_code() {
        let mut custom = Workflow::of(WorkflowPreset::Custom);
        assert_eq!(custom.validate(), [WorkflowIssue::NoEntry]);

        custom.set_stage(Stage::Dev, Trigger::Auto);
        assert_eq!(
            custom.validate(),
            [WorkflowIssue::NoEntry, WorkflowIssue::DevWithoutSpec]
        );

        custom.spec = true;
        custom.stages.clear();
        assert_eq!(custom.validate(), [WorkflowIssue::SpecWithoutDev]);

        custom.set_stage(Stage::Dev, Trigger::Auto);
        custom.set_stage(Stage::Fix, Trigger::Manual);
        assert_eq!(custom.validate(), [WorkflowIssue::FixWithoutReview]);

        custom.change_request = Some(entry());
        assert_eq!(
            custom.validate(),
            [
                WorkflowIssue::FixWithoutReview,
                WorkflowIssue::ChangeRequestNeedsReview
            ]
        );

        custom.set_stage(Stage::Review, Trigger::Auto);
        assert_eq!(custom.validate(), []);
        let codes: Vec<_> = [
            WorkflowIssue::NoEntry,
            WorkflowIssue::DevWithoutSpec,
            WorkflowIssue::SpecWithoutDev,
            WorkflowIssue::FixWithoutReview,
            WorkflowIssue::ChangeRequestNeedsReview,
        ]
        .iter()
        .map(|i| i.code())
        .collect();
        assert_eq!(
            codes,
            [
                "workflow_no_entry",
                "workflow_dev_without_spec",
                "workflow_spec_without_dev",
                "workflow_fix_without_review",
                "workflow_change_request_needs_review",
            ]
        );
    }

    #[test]
    fn stages_stay_in_pipeline_order() {
        let mut w = Workflow::of(WorkflowPreset::Custom);
        w.set_stage(Stage::Merge, Trigger::Manual);
        w.set_stage(Stage::Review, Trigger::Auto);
        w.set_stage(Stage::Review, Trigger::Manual);
        assert_eq!(
            w.stages,
            [
                stage(Stage::Review, Trigger::Manual),
                stage(Stage::Merge, Trigger::Manual)
            ]
        );
    }

    #[test]
    fn forks_and_drafts_are_skipped_unless_the_entry_allows_them() {
        let none: Vec<String> = Vec::new();
        let mut c = candidate(&none);
        c.from_fork = true;
        assert_eq!(entry().screen(&c), Err(Skip::Fork));
        let allowed = ChangeEntry {
            include_forks: true,
            ..entry()
        };
        assert_eq!(allowed.screen(&c), Ok(()));
        c.from_fork = false;
        c.draft = true;
        assert_eq!(entry().screen(&c), Err(Skip::Draft));
        let allowed = ChangeEntry {
            include_drafts: true,
            ..entry()
        };
        assert_eq!(allowed.screen(&c), Ok(()));
    }

    #[test]
    fn labels_authors_and_branches_each_narrow_the_set() {
        let labels = vec!["Needs-Review".to_string(), "docs".to_string()];
        let c = candidate(&labels);
        let with = |f: fn(&mut ChangeEntry)| {
            let mut e = entry();
            f(&mut e);
            e.screen(&c)
        };
        assert_eq!(with(|_| {}), Ok(()));
        assert_eq!(
            with(|e| e.labels = vec!["needs-review".into()]),
            Ok(()),
            "labels compare without case"
        );
        assert_eq!(with(|e| e.labels = vec!["wip".into()]), Err(Skip::Label));
        assert_eq!(with(|e| e.authors = vec!["alice".into()]), Ok(()));
        assert_eq!(with(|e| e.authors = vec!["bob".into()]), Err(Skip::Author));
        assert_eq!(with(|e| e.branches = vec!["feature/**".into()]), Ok(()));
        assert_eq!(
            with(|e| e.branches = vec!["feature/**".into(), "!feature/retries".into()]),
            Err(Skip::Branch)
        );
        assert_eq!(
            with(|e| e.branches = vec!["release/*".into()]),
            Err(Skip::Branch)
        );
    }

    #[test]
    fn the_path_filter_needs_one_touched_included_path() {
        let e = ChangeEntry {
            paths: vec!["services/api/**".into(), "!**/*.md".into()],
            ..entry()
        };
        assert!(e.needs_files());
        assert!(e.touches(&["services/api/src/lib.rs".into()]));
        assert!(!e.touches(&["services/api/README.md".into()]));
        assert!(!e.touches(&["web/app.ts".into()]));
        assert!(!entry().needs_files());
    }

    #[test]
    fn the_resolved_workflow_round_trips_as_json() {
        let w = Workflow::of(WorkflowPreset::ReviewOnly);
        let text = serde_json::to_string(&w).unwrap();
        assert_eq!(serde_json::from_str::<Workflow>(&text).unwrap(), w);
        assert!(text.contains(r#""preset":"review_only""#));
        assert!(text.contains(r#""trigger_label":"saka:review""#));
    }

    #[test]
    fn filter_text_is_bounded_and_printable() {
        assert!(filter_text_ok("needs-review"));
        assert!(!filter_text_ok(""));
        assert!(!filter_text_ok(&"x".repeat(MAX_FILTER_BYTES + 1)));
        assert!(!filter_text_ok("a\nb"));
    }
}
