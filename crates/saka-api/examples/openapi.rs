//! Writes the OpenAPI document generated from the code to `crates/saka-api/openapi.json`;
//! with `--check`, exits 1 when the committed file differs.

use std::io::Write;
use std::path::Path;
use std::process::ExitCode;

fn main() -> ExitCode {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("openapi.json");
    let generated = saka_api::openapi_json();
    let check = std::env::args().any(|a| a == "--check");
    let mut err = std::io::stderr();
    if check {
        let committed = std::fs::read_to_string(&path).unwrap_or_default();
        if committed == generated {
            return ExitCode::SUCCESS;
        }
        let _ = writeln!(
            err,
            "{} is out of date; run `cargo run -p saka-api --example openapi`",
            path.display()
        );
        return ExitCode::FAILURE;
    }
    match std::fs::write(&path, generated) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            let _ = writeln!(err, "write {}: {e}", path.display());
            ExitCode::FAILURE
        }
    }
}
