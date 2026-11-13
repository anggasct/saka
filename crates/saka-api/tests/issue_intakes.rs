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

impl Api {
    fn post(&self, path: &str, key: &str, body: &Value) -> Reply {
        self.sim.block_on(async {
            let response = self
                .app
                .clone()
                .oneshot(
                    Request::builder()
                        .method("POST")
                        .uri(path)
                        .header("host", "127.0.0.1:7420")
                        .header("content-type", "application/json")
                        .header("idempotency-key", key)
                        .body(Body::from(body.to_string()))
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

    fn list_url(&self) -> String {
        format!("/api/v1/projects/{}/issue-intakes", self.project)
    }
}

fn problem(r: &Reply, status: StatusCode, code: &str) {
    assert_eq!(r.status, status, "{}", r.body);
    assert_eq!(r.body["code"], code, "{}", r.body);
}

const BUGFIX: &str = "[workflow.issue]\n\n[workflow.issue.bugfix]\ndone_when = [\"cargo test\"]\nregression = [\"cargo test bug\"]\n";

#[test]
fn an_operator_puts_an_issue_in_and_a_replay_returns_the_same_intake() {
    let api = Api::new(BUGFIX);
    api.sim.world.open_issue(
        SHOP_REPO,
        saka_testkit::IssueSpec::new("Crash on start", "alice").labels(&["bug"]),
    );
    let created = api.post(&api.list_url(), "k-1", &json!({ "number": 1 }));
    assert_eq!(created.status, StatusCode::CREATED, "{}", created.body);
    let b = &created.body;
    assert_eq!(
        (b["state"].as_str(), b["cause"].as_str()),
        (Some("accepted"), Some("operator"))
    );
    assert_eq!(
        (b["lane"].as_str(), b["kind"].as_str()),
        (Some("bugfix"), Some("bugfix"))
    );
    assert_eq!(b["issue"]["number"], 1);
    assert_eq!(b["repo"]["name"], "shop");
    assert!(!b["actor"].as_str().unwrap().is_empty());
    let replay = api.post(&api.list_url(), "k-1", &json!({ "number": 1 }));
    assert_eq!(replay.status, StatusCode::CREATED);
    assert_eq!(replay.body["id"], created.body["id"]);
    let listed = api.get(&api.list_url());
    assert_eq!(listed.body["items"].as_array().unwrap().len(), 1);
}

#[test]
fn putting_an_issue_in_checks_the_issue_the_repo_and_the_workflow() {
    let api = Api::new(BUGFIX);
    api.sim.world.open_issue(
        SHOP_REPO,
        saka_testkit::IssueSpec::new("A pull request", "alice"),
    );
    api.sim.world.mark_issue_as_change_request(SHOP_REPO, 1);
    let closed = api
        .sim
        .world
        .open_issue(SHOP_REPO, saka_testkit::IssueSpec::new("Closed", "alice"));
    api.sim.world.close_issue(SHOP_REPO, closed, "alice");

    let missing = api.post(&api.list_url(), "k-1", &json!({ "number": 99 }));
    problem(&missing, StatusCode::NOT_FOUND, "issue_not_found");
    let pr = api.post(&api.list_url(), "k-2", &json!({ "number": 1 }));
    problem(&pr, StatusCode::UNPROCESSABLE_ENTITY, "validation_failed");
    assert_eq!(pr.body["params"]["errors"][0]["field"], "number");
    assert_eq!(pr.body["params"]["errors"][0]["code"], "is_change_request");
    let done = api.post(&api.list_url(), "k-3", &json!({ "number": closed }));
    problem(&done, StatusCode::UNPROCESSABLE_ENTITY, "validation_failed");
    let repo = api.post(
        &api.list_url(),
        "k-4",
        &json!({ "number": 2, "repo": "nope" }),
    );
    problem(&repo, StatusCode::UNPROCESSABLE_ENTITY, "validation_failed");
    assert_eq!(repo.body["params"]["errors"][0]["field"], "repo");
    let zero = api.post(&api.list_url(), "k-5", &json!({ "number": 0 }));
    problem(&zero, StatusCode::UNPROCESSABLE_ENTITY, "validation_failed");
    let extra = api.post(
        &api.list_url(),
        "k-6",
        &json!({ "number": 2, "labels": [] }),
    );
    problem(
        &extra,
        StatusCode::UNPROCESSABLE_ENTITY,
        "validation_failed",
    );

    let plain = Api::new("");
    plain
        .sim
        .world
        .open_issue(SHOP_REPO, saka_testkit::IssueSpec::new("Crash", "alice"));
    let off = plain.post(&plain.list_url(), "k-1", &json!({ "number": 1 }));
    problem(&off, StatusCode::CONFLICT, "entry_point_disabled");
    assert_eq!(off.body["params"]["entry"], "issue");
}

#[test]
fn an_issue_with_a_live_intake_cannot_be_put_in_twice() {
    let api = Api::new(BUGFIX);
    api.sim
        .world
        .open_issue(SHOP_REPO, saka_testkit::IssueSpec::new("Crash", "alice"));
    let first = api.post(&api.list_url(), "k-1", &json!({ "number": 1 }));
    let again = api.post(&api.list_url(), "k-2", &json!({ "number": 1 }));
    problem(&again, StatusCode::CONFLICT, "intake_active");
    assert_eq!(again.body["params"]["intake_id"], first.body["id"]);
}

#[test]
fn dismissing_before_approval_withdraws_the_spec_and_after_approval_is_refused() {
    let mut api = Api::new(BUGFIX);
    api.sim.world.open_issue(
        SHOP_REPO,
        saka_testkit::IssueSpec::new("Crash on start", "alice").labels(&["bug"]),
    );
    let first = api.post(&api.list_url(), "k-1", &json!({ "number": 1 }));
    let id = first.body["id"].as_str().unwrap().to_string();
    assert!(api.sim.run_until(8_000, |s| {
        s.engine()
            .issue_intakes(
                s.project_id,
                &[],
                &saka_engine::Page {
                    after: None,
                    limit: 10,
                },
            )
            .unwrap()
            .items
            .iter()
            .any(|i| i.intake.state.as_str() == "awaiting_approval")
    }));
    let spec_id = api.get(&format!("/api/v1/issue-intakes/{id}")).body["spec"]["id"]
        .as_str()
        .unwrap()
        .to_string();

    let dismissed = api.post(
        &format!("/api/v1/issue-intakes/{id}/actions/dismiss"),
        "k-2",
        &json!({ "reason": "duplicate of #7" }),
    );
    assert_eq!(dismissed.status, StatusCode::OK, "{}", dismissed.body);
    assert_eq!(dismissed.body["state"], "dismissed");
    assert_eq!(dismissed.body["spec"]["state"], "withdrawn");
    let spec = api.get(&format!("/api/v1/specs/{spec_id}"));
    assert_eq!(spec.body["state"], "withdrawn");
    assert!(api.sim.run_until(8_000, |s| {
        s.world
            .issue_comments()
            .iter()
            .any(|c| c.body.contains("not planned): duplicate of #7"))
    }));

    let twice = api.post(
        &format!("/api/v1/issue-intakes/{id}/actions/dismiss"),
        "k-3",
        &json!({}),
    );
    problem(&twice, StatusCode::CONFLICT, "intake_closed");

    api.sim.world.open_issue(
        SHOP_REPO,
        saka_testkit::IssueSpec::new("Second", "alice").labels(&["bug"]),
    );
    let second = api.post(&api.list_url(), "k-4", &json!({ "number": 2 }));
    let id2 = second.body["id"].as_str().unwrap().to_string();
    assert!(api.sim.run_until(8_000, |s| {
        s.engine()
            .issue_intake(
                id2.parse::<uuid::Uuid>()
                    .map(saka_core::ids::IssueIntakeId)
                    .unwrap(),
            )
            .unwrap()
            .intake
            .state
            .as_str()
            == "awaiting_approval"
    }));
    let detail = api.get(&format!("/api/v1/issue-intakes/{id2}"));
    let spec2: saka_core::ids::SpecId = detail.body["spec"]["id"]
        .as_str()
        .unwrap()
        .parse()
        .map(saka_core::ids::SpecId)
        .unwrap();
    let revision = api.sim.engine().spec(spec2).unwrap().unwrap().revision.id;
    api.sim.approve_spec(spec2, revision).unwrap();
    assert!(api.sim.run_until(8_000, |s| {
        s.engine()
            .issue_intake(
                id2.parse::<uuid::Uuid>()
                    .map(saka_core::ids::IssueIntakeId)
                    .unwrap(),
            )
            .unwrap()
            .intake
            .is_approved()
    }));
    let late = api.post(
        &format!("/api/v1/issue-intakes/{id2}/actions/dismiss"),
        "k-5",
        &json!({}),
    );
    problem(&late, StatusCode::CONFLICT, "intake_already_approved");
    let long = api.post(
        &format!("/api/v1/issue-intakes/{id}/actions/dismiss"),
        "k-6",
        &json!({ "reason": "x".repeat(501) }),
    );
    problem(&long, StatusCode::UNPROCESSABLE_ENTITY, "validation_failed");
}

#[test]
fn retrying_a_failed_intake_starts_over_and_the_fourth_retry_is_refused() {
    let mut api = Api::new(BUGFIX);
    api.sim.world.open_issue(
        SHOP_REPO,
        saka_testkit::IssueSpec::new("Crash on start", "alice").labels(&["bug"]),
    );
    let created = api.post(&api.list_url(), "k-1", &json!({ "number": 1 }));
    let id = created.body["id"].as_str().unwrap().to_string();
    let intake_id = saka_core::ids::IssueIntakeId(id.parse().unwrap());
    let not_failed = api.post(
        &format!("/api/v1/issue-intakes/{id}/actions/retry"),
        "k-2",
        &json!({}),
    );
    problem(&not_failed, StatusCode::CONFLICT, "intake_not_failed");

    for attempt in 1_u32..=3 {
        assert!(api.sim.run_until(8_000, |s| {
            s.engine()
                .issue_intake(intake_id)
                .unwrap()
                .intake
                .state
                .as_str()
                == "awaiting_approval"
        }));
        let current = api.sim.engine().issue_intake(intake_id).unwrap().intake;
        let failing = issue_intake::decide(
            Some(&current),
            Input::Fail {
                code: "draft_failed".into(),
                params: json!({}),
            },
            &mut Ctx {
                now: api.sim.engine().now(),
                ids: &mut Seq(9_000 + u128::from(attempt)),
            },
        )
        .unwrap();
        let failed =
            issue_intake::evolve_all(Some(current), &failing, api.sim.engine().now()).unwrap();
        api.sim.store_issue_intake(&failed);
        let retried = api.post(
            &format!("/api/v1/issue-intakes/{id}/actions/retry"),
            &format!("retry-{attempt}"),
            &json!({}),
        );
        assert_eq!(retried.status, StatusCode::ACCEPTED, "{}", retried.body);
        assert_eq!(retried.body["retries"], attempt);
        assert_eq!(retried.body["state"], "accepted");
    }
    assert!(api.sim.run_until(8_000, |s| {
        s.engine()
            .issue_intake(intake_id)
            .unwrap()
            .intake
            .state
            .as_str()
            == "awaiting_approval"
    }));
    let current = api.sim.engine().issue_intake(intake_id).unwrap().intake;
    let failing = issue_intake::decide(
        Some(&current),
        Input::Fail {
            code: "draft_failed".into(),
            params: json!({}),
        },
        &mut Ctx {
            now: api.sim.engine().now(),
            ids: &mut Seq(9_999),
        },
    )
    .unwrap();
    let failed = issue_intake::evolve_all(Some(current), &failing, api.sim.engine().now()).unwrap();
    api.sim.store_issue_intake(&failed);
    let exhausted = api.post(
        &format!("/api/v1/issue-intakes/{id}/actions/retry"),
        "retry-4",
        &json!({}),
    );
    problem(&exhausted, StatusCode::CONFLICT, "intake_retries_exhausted");
}
