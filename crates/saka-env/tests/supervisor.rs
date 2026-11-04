#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "tests fail loudly on a broken setup"
)]

use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::time::{Duration, Instant};

use saka_core::diagnostics::SupervisorProbe;
use saka_env::probe_systemd;

fn fake_systemd_run(dir: &Path, body: &str) -> std::path::PathBuf {
    let path = dir.join("systemd-run");
    std::fs::write(
        &path,
        format!("#!/bin/sh\necho \"$*\" > \"$(dirname \"$0\")/argv\"\n{body}\n"),
    )
    .unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path
}

fn block_on<F: std::future::Future>(f: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(f)
}

#[test]
fn the_probe_runs_true_in_a_transient_user_unit() {
    let dir = tempfile::tempdir().unwrap();
    let ok = fake_systemd_run(dir.path(), "exit 0");
    assert_eq!(
        block_on(probe_systemd(ok.as_os_str(), Duration::from_secs(5))),
        SupervisorProbe::Available
    );
    assert_eq!(
        std::fs::read_to_string(dir.path().join("argv"))
            .unwrap()
            .trim(),
        "--user --wait --pipe --quiet --collect -- /bin/true"
    );
}

#[test]
fn a_user_manager_that_cannot_run_the_unit_is_unavailable_with_its_exit_code() {
    let dir = tempfile::tempdir().unwrap();
    let failing = fake_systemd_run(
        dir.path(),
        "echo 'Failed to connect to bus: No medium found' >&2\nexit 1",
    );
    assert_eq!(
        block_on(probe_systemd(failing.as_os_str(), Duration::from_secs(5))),
        SupervisorProbe::Unavailable {
            exit_code: Some(1),
            detail: "Failed to connect to bus: No medium found".into(),
        }
    );
    let missing = dir.path().join("no-such-systemd-run");
    assert!(matches!(
        block_on(probe_systemd(missing.as_os_str(), Duration::from_secs(5))),
        SupervisorProbe::Unavailable {
            exit_code: None,
            ..
        }
    ));
}

#[test]
fn a_probe_that_hangs_is_cut_at_its_timeout() {
    let dir = tempfile::tempdir().unwrap();
    let hanging = fake_systemd_run(dir.path(), "exec sleep 60");
    let started = Instant::now();
    let got = block_on(probe_systemd(
        hanging.as_os_str(),
        Duration::from_millis(200),
    ));
    assert!(started.elapsed() < Duration::from_secs(5));
    assert_eq!(
        got,
        SupervisorProbe::Unavailable {
            exit_code: None,
            detail: "timed out".into(),
        }
    );
}
