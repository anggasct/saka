#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "tests fail loudly on a broken setup"
)]

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use saka_api::{ApiConfig, AppState, router};
use saka_core::Ctx;
use saka_core::ids::{IdSource, ProjectId};
use saka_core::issue_intake::{self, Input, IssueIntake, NewIntake};
use saka_core::vocab::{IssueCause, IssueLane, WorkKind};
use saka_testkit::{Layout, SHOP_REPO, Sim, SimConfig, repo_entry, saka_toml};
use serde_json::{Value, json};
use tower::ServiceExt;

const WORKFLOW: &str = "[workflow.issue]\ntrigger = \"auto\"\ntrigger_label = \"run-saka\"\nkind_labels = { bug = \"bugfix\" }\ndefault_kind = \"chore\"\nmax_open = 7\n\n[workflow.issue.bugfix]\ndone_when = [\"cargo test\"]\nregression = [\"cargo test bug\"]\n";

struct Seq(u128);

impl IdSource for Seq {
    fn next_id(&mut self) -> uuid::Uuid {
        self.0 += 1;
        uuid::Uuid::from_u128(self.0)
    }
}

struct Api {
    sim: Sim,
    app: Router,
    project: ProjectId,
}

struct Reply {
    status: StatusCode,
    body: Value,
}

impl Api {
    fn new(workflow: &str) -> Api {
        let mut text = saka_toml("shop", &[repo_entry("shop", SHOP_REPO, "")]);
        text.push('\n');
        text.push_str(workflow);
        let sim = Sim::multi_repo(
            SimConfig::default(),
            &Layout {
                saka_toml: text,
                ..Layout::default()
            },
        );
        let state = AppState::new(ApiConfig::new("0.1.0-test"));
        state.set_engine(sim.engine());
        let project = sim.project_id;
        Api {
            sim,
            app: router(state),
            project,
        }
    }

    fn get(&self, path: &str) -> Reply {
        self.sim.block_on(async {
            let response = self
                .app
                .clone()
                .oneshot(
                    Request::get(path)
                        .header("host", "127.0.0.1:7420")
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            let status = response.status();
            let bytes = response.into_body().collect().await.unwrap().to_bytes();
            Reply {
                status,
                body: serde_json::from_slice(&bytes).unwrap_or(Value::Null),
            }
        })
    }

    fn seed(&mut self, number: u64, lane: IssueLane, kind: WorkKind) -> IssueIntake {
        let engine = self.sim.engine();
        let project = engine.project(self.project).unwrap().unwrap();
        let repo_id = project
            .repos
            .iter()
            .find(|r| r.full_name == SHOP_REPO)
            .unwrap()
            .id;
        self.sim.clock.advance(1000);
        let now = engine.now();
        let mut ids = Seq(u128::from(number) * 1000);
        let mut ctx = Ctx { now, ids: &mut ids };
        let new = NewIntake {
            project_id: self.project,
            repo_id,
            number,
            url: format!("https://github.com/acme/shop/issues/{number}"),
            title: format!("Issue {number}"),
            author: "alice".into(),
            lane,
            kind,
            cause: IssueCause::Label,
            actor: "maintainer".into(),
        };
        let events = issue_intake::decide(None, Input::Accept(new), &mut ctx).unwrap();
        let intake = issue_intake::evolve_all(None, &events, now).unwrap();
        self.store(&intake);
        intake
    }

    fn apply(&mut self, intake: &IssueIntake, input: Input) -> IssueIntake {
        let engine = self.sim.engine();
        let now = engine.now();
        let mut ids = Seq(0);
        let mut ctx = Ctx { now, ids: &mut ids };
        let events = issue_intake::decide(Some(intake), input, &mut ctx).unwrap();
        let next = issue_intake::evolve_all(Some(intake.clone()), &events, now).unwrap();
        self.store(&next);
        next
    }

    fn store(&self, intake: &IssueIntake) {
        self.sim.store_issue_intake(intake);
    }
}

#[test]
fn the_project_shows_its_issue_entry_or_none() {
    let with = Api::new(WORKFLOW);
    let project = with.get(&format!("/api/v1/projects/{}", with.project));
    assert_eq!(project.status, StatusCode::OK, "{}", project.body);
    let issue = &project.body["workflow"]["issue"];
    assert_eq!(issue["trigger"], "auto");
    assert_eq!(issue["trigger_label"], "run-saka");
    assert_eq!(issue["default_kind"], "chore");
    assert_eq!(issue["max_open"], 7);
    assert_eq!(issue["kind_labels"], json!({ "bug": "bugfix" }));
    assert_eq!(issue["include_confidential"], false);
    assert_eq!(
        issue["bugfix"],
        json!({ "done_when": ["cargo test"], "regression": ["cargo test bug"] })
    );
    let without = Api::new("");
    let project = without.get(&format!("/api/v1/projects/{}", without.project));
    assert!(
        project.body["workflow"]["issue"].is_null(),
        "{}",
        project.body
    );
}

#[test]
fn intakes_are_listed_newest_first_and_filtered_by_state() {
    let mut api = Api::new(WORKFLOW);
    let first = api.seed(1, IssueLane::Bugfix, WorkKind::Bugfix);
    let second = api.seed(2, IssueLane::Draft, WorkKind::Chore);
    api.apply(
        &second,
        Input::Fail {
            code: "draft_failed".into(),
            params: json!({ "draft_state": "failed" }),
        },
    );
    let base = format!("/api/v1/projects/{}/issue-intakes", api.project);

    let all = api.get(&base);
    assert_eq!(all.status, StatusCode::OK, "{}", all.body);
    let numbers: Vec<u64> = all.body["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["issue"]["number"].as_u64().unwrap())
        .collect();
    assert_eq!(numbers, [2, 1]);
    assert!(all.body["next_cursor"].is_null());

    let failed = api.get(&format!("{base}?state=failed"));
    assert_eq!(failed.body["items"].as_array().unwrap().len(), 1);
    assert_eq!(failed.body["items"][0]["id"], second.id.to_string());
    let both = api.get(&format!("{base}?state=accepted,failed"));
    assert_eq!(both.body["items"].as_array().unwrap().len(), 2);
    let none = api.get(&format!("{base}?state=done"));
    assert!(none.body["items"].as_array().unwrap().is_empty());
    assert_eq!(first.state.as_str(), "accepted");
}

#[test]
fn a_page_ends_with_a_cursor_that_continues_the_list() {
    let mut api = Api::new(WORKFLOW);
    for number in 1..=3 {
        api.seed(number, IssueLane::Bugfix, WorkKind::Bugfix);
    }
    let base = format!("/api/v1/projects/{}/issue-intakes", api.project);
    let page = api.get(&format!("{base}?limit=2"));
    assert_eq!(page.body["items"].as_array().unwrap().len(), 2);
    let cursor = page.body["next_cursor"].as_str().unwrap().to_string();
    let rest = api.get(&format!("{base}?limit=2&cursor={cursor}"));
    let numbers: Vec<u64> = rest.body["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["issue"]["number"].as_u64().unwrap())
        .collect();
    assert_eq!(numbers, [1]);
    assert!(rest.body["next_cursor"].is_null());
}

#[test]
fn an_unknown_state_or_project_is_refused() {
    let api = Api::new(WORKFLOW);
    let base = format!("/api/v1/projects/{}/issue-intakes", api.project);
    let bad = api.get(&format!("{base}?state=paused"));
    assert_eq!(bad.status, StatusCode::UNPROCESSABLE_ENTITY, "{}", bad.body);
    assert_eq!(bad.body["code"], "validation_failed");
    let missing = api.get(&format!(
        "/api/v1/projects/{}/issue-intakes",
        uuid::Uuid::from_u128(77)
    ));
    assert_eq!(missing.status, StatusCode::NOT_FOUND);
}

#[test]
fn the_detail_names_the_issue_the_repo_the_spec_the_draft_and_the_failure() {
    let mut api = Api::new(WORKFLOW);
    let intake = api.seed(42, IssueLane::Draft, WorkKind::Chore);
    let draft = api
        .sim
        .create_draft("docs/specs/issue-42.md", "Fix the crash", &[])
        .unwrap();
    let linked = api.apply(
        &intake,
        Input::DraftStarted {
            draft_id: draft.draft.id,
        },
    );
    let detail = api.get(&format!("/api/v1/issue-intakes/{}", linked.id));
    assert_eq!(detail.status, StatusCode::OK, "{}", detail.body);
    let b = &detail.body;
    assert_eq!(b["state"], "drafting");
    assert_eq!(
        (b["lane"].as_str(), b["kind"].as_str()),
        (Some("draft"), Some("chore"))
    );
    assert_eq!(
        (b["cause"].as_str(), b["actor"].as_str()),
        (Some("label"), Some("maintainer"))
    );
    assert_eq!(b["issue"]["number"], 42);
    assert_eq!(b["issue"]["title"], "Issue 42");
    assert_eq!(b["issue"]["author"], "alice");
    assert_eq!(b["repo"]["name"], "shop");
    assert_eq!(b["repo"]["full_name"], SHOP_REPO);
    assert!(b["spec"].is_null());
    assert_eq!(b["draft"]["id"], draft.draft.id.to_string());
    assert_eq!(b["draft"]["state"], "queued");
    assert!(b["failure"].is_null());
    assert_eq!(b["retries"], 0);

    let spec = api.sim.spec_id;
    let ready = api.apply(&linked, Input::SpecReady { spec_id: spec });
    let detail = api.get(&format!("/api/v1/issue-intakes/{}", ready.id));
    assert_eq!(detail.body["state"], "awaiting_approval");
    assert_eq!(detail.body["spec"]["id"], spec.to_string());
    assert!(
        detail.body["spec"]["path"]
            .as_str()
            .unwrap()
            .ends_with(".md")
    );

    let failed = api.apply(
        &ready,
        Input::Fail {
            code: "spec_lint_failed".into(),
            params: json!({ "line": 3 }),
        },
    );
    let detail = api.get(&format!("/api/v1/issue-intakes/{}", failed.id));
    assert_eq!(
        detail.body["failure"],
        json!({ "code": "spec_lint_failed", "params": { "line": 3 } })
    );
}

#[test]
fn an_unknown_or_malformed_intake_is_not_found() {
    let api = Api::new(WORKFLOW);
    for id in [uuid::Uuid::from_u128(5).to_string(), "nope".to_string()] {
        let r = api.get(&format!("/api/v1/issue-intakes/{id}"));
        assert_eq!(r.status, StatusCode::NOT_FOUND, "{id}");
        assert_eq!(r.body["code"], "not_found");
    }
}
