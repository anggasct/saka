#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "tests fail loudly on a broken setup"
)]

use axum::Router;
use axum::body::Body;
use axum::http::{HeaderMap, Method, Request, StatusCode};
use http_body_util::BodyExt;
use saka_api::{ApiConfig, AppState, router};
use saka_ports::DocWrite;
use saka_testkit::fixtures::TWO_STEP_SPEC;
use saka_testkit::{Sim, SimConfig, git_blob, one_step_spec};
use serde_json::{Value, json};
use tower::ServiceExt;

struct Api {
    sim: Sim,
    app: Router,
    project: String,
    next_key: u32,
}

struct Reply {
    status: StatusCode,
    headers: HeaderMap,
    body: Value,
}

const CORPUS: [(&str, &str); 10] = [
    ("lf", "# A\nline\n"),
    ("crlf", "# A\r\nline\r\n"),
    ("mixed", "# A\r\nline\nlast\r\n"),
    ("lone-cr", "# A\rline\r"),
    ("bom", "\u{feff}# A\nline\n"),
    ("no-newline", "# A\nlast line"),
    ("empty", ""),
    ("unicode", "# Ünïcödé 🚀\ne\u{301} combining\n"),
    ("spaces", "\tindented\ntrailing   \n\n\n"),
    ("nbsp", "a\u{a0}b \u{2028} c\n"),
];

impl Api {
    fn new() -> Api {
        let sim = Sim::new(SimConfig::default());
        let state = AppState::new(ApiConfig::new("0.1.0-test"));
        state.set_engine(sim.engine());
        let app = router(state);
        let project = sim.project_id.0.to_string();
        Api {
            sim,
            app,
            project,
            next_key: 0,
        }
    }

    fn send(&self, req: Request<Body>) -> Reply {
        self.sim.block_on(async {
            let response = self.app.clone().oneshot(req).await.unwrap();
            let status = response.status();
            let headers = response.headers().clone();
            let bytes = response.into_body().collect().await.unwrap().to_bytes();
            let body = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
            Reply {
                status,
                headers,
                body,
            }
        })
    }

    fn get(&self, path: &str) -> Reply {
        self.send(
            Request::get(path)
                .header("host", "127.0.0.1:7420")
                .body(Body::empty())
                .unwrap(),
        )
    }

    fn file_url(&self, path: &str) -> String {
        let encoded: String = path
            .bytes()
            .map(|b| match b {
                b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'/' => {
                    (b as char).to_string()
                }
                _ => format!("%{b:02X}"),
            })
            .collect();
        format!("/api/v1/projects/{}/file?path={encoded}", self.project)
    }

    fn read(&self, path: &str) -> Reply {
        self.get(&self.file_url(path))
    }

    fn key(&mut self) -> String {
        self.next_key += 1;
        format!("key-{}", self.next_key)
    }

    fn put_raw(
        &self,
        path: &str,
        key: Option<&str>,
        headers: &[(&str, &str)],
        body: &str,
    ) -> Reply {
        let mut req = Request::builder()
            .method(Method::PUT)
            .uri(self.file_url(path))
            .header("host", "127.0.0.1:7420")
            .header("content-type", "application/json");
        if let Some(k) = key {
            req = req.header("idempotency-key", k);
        }
        for (k, v) in headers {
            req = req.header(*k, *v);
        }
        self.send(req.body(Body::from(body.to_string())).unwrap())
    }

    fn put(&mut self, path: &str, headers: &[(&str, &str)], content: &str) -> Reply {
        let key = self.key();
        let body = json!({ "content": content }).to_string();
        self.put_raw(path, Some(&key), headers, &body)
    }

    fn create(&mut self, path: &str, content: &str) -> Reply {
        self.put(path, &[("if-none-match", "*")], content)
    }

    fn update(&mut self, path: &str, blob: &str, content: &str) -> Reply {
        let tag = format!("\"{blob}\"");
        self.put(path, &[("if-match", &tag)], content)
    }
}

fn problem(r: &Reply, status: StatusCode, code: &str) {
    assert_eq!(r.status, status, "{}", r.body);
    assert_eq!(r.body["code"], code, "{}", r.body);
}

fn invalid(r: &Reply, field: &str, code: &str) {
    problem(r, StatusCode::UNPROCESSABLE_ENTITY, "validation_failed");
    assert_eq!(
        r.body["params"]["errors"][0],
        json!({ "field": field, "code": code }),
        "{}",
        r.body
    );
}

fn etag(r: &Reply) -> String {
    r.headers["etag"].to_str().unwrap().to_string()
}

#[test]
fn the_tree_lists_markdown_files_of_the_project_folder_with_their_blobs() {
    let api = Api::new();
    let w = &api.sim.world;
    w.put_doc("docs/a.md", b"# A\n");
    w.put_doc("docs/specs/b.MD", b"# B\n");
    w.put_doc("docs/x.markdown", b"# X\n");
    w.put_doc("docs/c.txt", b"text");
    w.put_doc("src/d.md", b"# D\n");
    w.put_doc("docs/l.md", b"a.md");
    w.put_doc_link("docs/l.md");

    let r = api.get(&format!("/api/v1/projects/{}/files", api.project));
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    assert_eq!(r.body["commit"], w.base_head());
    assert_eq!(r.body["base_branch"], "main");
    assert_eq!(r.body["project_path"], "docs");
    assert_eq!(r.body["truncated"], false);
    let files = r.body["files"].as_array().unwrap();
    let paths: Vec<&str> = files.iter().map(|f| f["path"].as_str().unwrap()).collect();
    assert_eq!(paths, ["docs/a.md", "docs/specs/b.MD", "docs/x.markdown"]);
    assert_eq!(files[0]["blob"], git_blob(b"# A\n"));
    assert_eq!(files[0]["bytes"], 4);

    let missing = api.get(&format!("/api/v1/projects/{}/files", uuid::Uuid::now_v7()));
    problem(&missing, StatusCode::NOT_FOUND, "not_found");
}

#[test]
fn a_file_is_read_byte_exact_with_its_blob_as_a_strong_etag() {
    let api = Api::new();
    for (name, text) in CORPUS {
        let path = format!("docs/corpus/{name}.md");
        api.sim.world.put_doc(&path, text.as_bytes());
        let r = api.read(&path);
        assert_eq!(r.status, StatusCode::OK, "{name}: {}", r.body);
        assert_eq!(r.body["content"].as_str().unwrap(), text, "{name}");
        assert_eq!(r.body["bytes"], text.len());
        assert_eq!(r.body["blob"], git_blob(text.as_bytes()));
        assert_eq!(r.body["commit"], api.sim.world.base_head());
        assert_eq!(etag(&r), format!("\"{}\"", git_blob(text.as_bytes())));
    }
}

#[test]
fn reading_refuses_bad_paths_and_files_the_editor_cannot_hold() {
    let api = Api::new();
    let w = &api.sim.world;
    w.put_doc("docs/big.md", &vec![b'a'; 2 * 1024 * 1024]);
    w.put_doc("docs/latin1.md", b"caf\xe9\n");
    w.put_doc("docs/link.md", b"a.md");
    w.put_doc_link("docs/link.md");

    invalid(&api.read("docs/../a.md"), "path", "invalid");
    invalid(&api.read("/docs/a.md"), "path", "invalid");
    invalid(&api.read("docs/.git/a.md"), "path", "invalid");
    invalid(&api.read("docs/a.txt"), "path", "not_markdown");
    invalid(&api.read("src/a.md"), "path", "outside_project");
    invalid(
        &api.get(&format!("/api/v1/projects/{}/file", api.project)),
        "path",
        "required",
    );
    problem(
        &api.read("docs/none.md"),
        StatusCode::NOT_FOUND,
        "not_found",
    );

    let big = api.read("docs/big.md");
    problem(&big, StatusCode::UNPROCESSABLE_ENTITY, "file_unsupported");
    assert_eq!(
        big.body["params"],
        json!({ "reason": "too_large", "bytes": 2 * 1024 * 1024, "max_bytes": 1024 * 1024 })
    );
    let latin = api.read("docs/latin1.md");
    problem(&latin, StatusCode::UNPROCESSABLE_ENTITY, "file_unsupported");
    assert_eq!(latin.body["params"], json!({ "reason": "not_utf8" }));
    let link = api.read("docs/link.md");
    problem(&link, StatusCode::UNPROCESSABLE_ENTITY, "file_unsupported");
    assert_eq!(link.body["params"], json!({ "reason": "not_regular" }));
}

#[test]
fn saving_creates_updates_or_leaves_a_file_and_records_one_event_per_commit() {
    let mut api = Api::new();
    let before = api.sim.engine().last_seq().unwrap();
    let text = "# Notes\r\nfirst\r\n";
    let created = api.create("docs/notes.md", text);
    assert_eq!(created.status, StatusCode::CREATED, "{}", created.body);
    let blob = git_blob(text.as_bytes());
    assert_eq!(created.body["blob"], blob);
    assert_eq!(created.body["committed"], true);
    assert_eq!(created.body["spec"], Value::Null);
    assert_eq!(created.body["spec_problem"], Value::Null);
    assert_eq!(created.body["commit"], api.sim.world.base_head());
    assert_eq!(etag(&created), format!("\"{blob}\""));
    assert_eq!(api.sim.world.doc("docs/notes.md").unwrap(), text.as_bytes());
    let commits = api.sim.world.doc_commits();
    assert_eq!(commits[0].author.name, "Saka Agent");
    assert_eq!(commits[0].branch, "main");

    let next = "# Notes\r\nsecond\r\n";
    let updated = api.update("docs/notes.md", &blob, next);
    assert_eq!(updated.status, StatusCode::OK, "{}", updated.body);
    assert_eq!(updated.body["committed"], true);
    let same = api.update("docs/notes.md", "0000", next);
    assert_eq!(same.status, StatusCode::OK, "{}", same.body);
    assert_eq!(same.body["committed"], false);
    assert_eq!(same.body["commit"], updated.body["commit"]);

    let events = api.sim.engine().events_after(before, 100).unwrap();
    let saved: Vec<&Value> = events
        .iter()
        .filter(|e| e.kind == "workspace.doc.committed")
        .map(|e| &e.payload)
        .collect();
    assert_eq!(saved.len(), 2);
    let parent = saved[0]["parent"].as_str().unwrap();
    assert_eq!(parent.len(), 40);
    assert_ne!(parent, created.body["commit"].as_str().unwrap());
    assert_eq!(
        *saved[0],
        json!({
            "project_id": api.project,
            "path": "docs/notes.md",
            "commit": created.body["commit"],
            "parent": parent,
            "blob": blob,
            "bytes": text.len(),
            "created": true,
        })
    );
    assert_eq!(saved[1]["created"], false);
}

#[test]
fn a_save_is_replayed_by_its_key_and_the_key_covers_the_file_path() {
    let mut api = Api::new();
    let body = json!({ "content": "# A\n" }).to_string();
    let first = api.put_raw(
        "docs/a.md",
        Some("save-a"),
        &[("if-none-match", "*")],
        &body,
    );
    assert_eq!(first.status, StatusCode::CREATED, "{}", first.body);
    let again = api.put_raw(
        "docs/a.md",
        Some("save-a"),
        &[("if-none-match", "*")],
        &body,
    );
    assert_eq!(again.status, StatusCode::CREATED);
    assert_eq!(again.body, first.body);
    assert_eq!(again.headers["idempotent-replayed"], "true");
    assert_eq!(etag(&again), etag(&first));
    assert_eq!(api.sim.world.doc_commits().len(), 1);
    let other = api.put_raw(
        "docs/b.md",
        Some("save-a"),
        &[("if-none-match", "*")],
        &body,
    );
    problem(
        &other,
        StatusCode::UNPROCESSABLE_ENTITY,
        "idempotency_key_reused",
    );
    let next = api.key();
    let none = api.put_raw("docs/a.md", Some(&next), &[], &body);
    problem(
        &none,
        StatusCode::PRECONDITION_REQUIRED,
        "precondition_required",
    );
    let both = api.put(
        "docs/a.md",
        &[("if-none-match", "*"), ("if-match", "\"x\"")],
        "# A\n",
    );
    invalid(&both, "If-None-Match", "exclusive");
    invalid(
        &api.put("docs/a.md", &[("if-none-match", "\"x\"")], "x"),
        "If-None-Match",
        "invalid",
    );
    invalid(
        &api.put("docs/a.md", &[("if-match", "x")], "x"),
        "If-Match",
        "invalid",
    );
    let missing_key = api.put_raw("docs/a.md", None, &[("if-none-match", "*")], &body);
    problem(
        &missing_key,
        StatusCode::BAD_REQUEST,
        "idempotency_key_required",
    );
    invalid(
        &api.put("src/a.md", &[("if-none-match", "*")], "x"),
        "path",
        "outside_project",
    );
}

#[test]
fn the_content_limit_counts_file_bytes_not_json_bytes() {
    let mut api = Api::new();
    let crlf = "\r\n".repeat(512 * 1024);
    let r = api.create("docs/crlf.md", &crlf);
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.body);
    assert_eq!(r.body["bytes"], 1024 * 1024);
    let over = api.create("docs/over.md", &"a".repeat(1024 * 1024 + 1));
    invalid(&over, "content", "too_large");
}

#[test]
fn conflicts_and_refusals_are_problems_that_replay_from_the_key() {
    let mut api = Api::new();
    let created = api.create("docs/a.md", "# A\n");
    let blob = created.body["blob"].as_str().unwrap().to_string();

    let stale = api.update("docs/a.md", &"9".repeat(40), "# Mine\n");
    problem(&stale, StatusCode::PRECONDITION_FAILED, "file_stale");
    assert_eq!(
        stale.body["params"],
        json!({ "blob": blob, "commit": api.sim.world.base_head() })
    );
    let exists = api.create("docs/a.md", "# Again\n");
    problem(&exists, StatusCode::PRECONDITION_FAILED, "file_stale");
    assert_eq!(exists.body["params"]["blob"], blob);
    let gone = api.update("docs/none.md", &blob, "# None\n");
    problem(&gone, StatusCode::PRECONDITION_FAILED, "file_stale");
    assert_eq!(gone.body["params"]["blob"], Value::Null);

    api.sim.world.script_doc_write(DocWrite::Rejected);
    let body = json!({ "content": "# Mine\n" }).to_string();
    let tag = format!("\"{blob}\"");
    let rejected = api.put_raw("docs/a.md", Some("rejected"), &[("if-match", &tag)], &body);
    problem(&rejected, StatusCode::CONFLICT, "file_push_rejected");
    assert_eq!(rejected.body["params"], json!({ "branch": "main" }));
    let replay = api.put_raw("docs/a.md", Some("rejected"), &[("if-match", &tag)], &body);
    problem(&replay, StatusCode::CONFLICT, "file_push_rejected");
    assert_eq!(replay.headers["idempotent-replayed"], "true");

    api.sim.world.script_doc_write(DocWrite::PathConflict {
        path: "docs/a.md".into(),
    });
    let conflict = api.create("docs/a.md/b.md", "# B\n");
    problem(&conflict, StatusCode::CONFLICT, "file_path_conflict");
    assert_eq!(conflict.body["params"], json!({ "path": "docs/a.md" }));

    api.sim.world.script_doc_write(DocWrite::NotRegular);
    let link = api.update("docs/a.md", &blob, "# B\n");
    problem(&link, StatusCode::UNPROCESSABLE_ENTITY, "file_unsupported");
    assert_eq!(link.body["params"], json!({ "reason": "not_regular" }));
    assert_eq!(api.sim.world.doc("docs/a.md").unwrap(), b"# A\n");
}

#[test]
fn saving_a_spec_registers_it_and_the_spec_is_found_by_path() {
    let mut api = Api::new();
    let text = one_step_spec("Saved from the editor", "");
    let r = api.create("docs/specs/saved.md", &text);
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.body);
    let spec = &r.body["spec"];
    assert_eq!(spec["path"], "docs/specs/saved.md");
    assert_eq!(spec["revision"]["source_commit"], r.body["commit"]);
    assert_eq!(
        spec["revision"]["content_hash"],
        saka_spec::content_hash(text.as_bytes())
    );
    assert_eq!(spec["lint"]["ok"], true);
    assert!(spec["acceptance_criteria"].is_array());
    assert_eq!(r.body["spec_problem"], Value::Null);

    let url = |path: &str| format!("/api/v1/projects/{}/specs?path={path}", api.project);
    let found = api.get(&url("docs/specs/saved.md"));
    assert_eq!(found.status, StatusCode::OK, "{}", found.body);
    assert_eq!(found.body["items"].as_array().unwrap().len(), 1);
    assert_eq!(found.body["items"][0]["id"], spec["id"]);
    let none = api.get(&url("docs/specs/other.md"));
    assert_eq!(none.body["items"], json!([]));
    invalid(&api.get(&url(&"a".repeat(1025))), "path", "too_long");

    let clash = api.create("docs/elsewhere/tasks-done.md", TWO_STEP_SPEC);
    assert_eq!(clash.status, StatusCode::CREATED, "{}", clash.body);
    assert_eq!(clash.body["spec"], Value::Null);
    assert_eq!(clash.body["spec_problem"]["code"], "spec_slug_conflict");
    assert!(api.sim.world.doc("docs/elsewhere/tasks-done.md").is_some());

    let odd = api.create("docs/specs/My Spec.md", &text);
    assert_eq!(odd.status, StatusCode::CREATED, "{}", odd.body);
    assert_eq!(
        odd.body["spec_problem"],
        json!({ "code": "validation_failed", "params": { "errors": [{ "field": "path", "code": "invalid" }] } })
    );
}
