#![allow(clippy::unwrap_used, reason = "tests fail loudly on a broken setup")]

use axum::body::Body;
use axum::http::Request;
use http_body_util::BodyExt;
use saka_api::{ApiConfig, AppState, router};
use saka_core::vocab::WorkItemState;
use saka_testkit::{CiScript, Sim, SimConfig};
use tower::ServiceExt;

const STEPS: usize = 8_000;

fn metrics(sim: &Sim) -> String {
    let state = AppState::new(ApiConfig::new("0.1.0-test"));
    state.set_engine(sim.engine());
    let app = router(state);
    sim.block_on(async {
        let req = Request::get("/metrics")
            .header("host", "127.0.0.1:7420")
            .body(Body::empty())
            .unwrap();
        let response = app.oneshot(req).await.unwrap();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        String::from_utf8_lossy(&bytes).into_owned()
    })
}

#[test]
fn a_panicked_job_is_counted_per_kind_in_the_metrics() {
    let mut cfg = SimConfig {
        ci: CiScript::Pass,
        ..SimConfig::default()
    };
    cfg.operator.retry = vec![];
    let mut sim = Sim::new(cfg);
    sim.world.panic_on("open_change_request", 1, "boom");
    sim.approve();
    assert!(sim.run_until(STEPS, |s| {
        s.task(1).item.state == WorkItemState::Blocked
    }));

    let text = metrics(&sim);
    assert!(
        text.contains("# TYPE saka_job_panics_total counter"),
        "{text}"
    );
    assert!(
        text.contains("saka_job_panics_total{kind=\"scm.open_change_request\"} 1"),
        "{text}"
    );
}
