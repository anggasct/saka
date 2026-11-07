use std::fmt::Display;

pub trait BestEffort {
    fn best_effort(self, step: &'static str);
}

impl<T, E: Display> BestEffort for Result<T, E> {
    fn best_effort(self, step: &'static str) {
        if let Err(e) = self {
            tracing::debug!(error = %e, step, "a best-effort step failed");
        }
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write;
    use std::sync::{Arc, Mutex};

    use super::*;

    #[derive(Clone, Default)]
    struct Captured(Arc<Mutex<Vec<u8>>>);

    impl Write for Captured {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    fn logged(run: impl FnOnce()) -> String {
        let captured = Captured::default();
        let writer = captured.clone();
        let subscriber = tracing_subscriber::fmt()
            .with_max_level(tracing::Level::DEBUG)
            .with_writer(move || writer.clone())
            .finish();
        tracing::subscriber::with_default(subscriber, run);
        let bytes = captured.0.lock().unwrap().clone();
        String::from_utf8(bytes).unwrap()
    }

    #[test]
    fn a_failed_step_is_logged_with_its_name_and_error() {
        let log = logged(|| Err::<(), _>("disk full").best_effort("remove the temp file"));
        assert!(log.contains("remove the temp file"));
        assert!(log.contains("disk full"));
    }

    #[test]
    fn a_step_that_worked_logs_nothing() {
        let log = logged(|| Ok::<(), &str>(()).best_effort("remove the temp file"));
        assert!(log.is_empty());
    }
}
