//! JSON log lines on stderr. Fields of the enclosing spans (`correlation_id`,
//! `work_item_id`, `job_kind`, …) are written at the top level of every line inside them.

use std::fmt;

use serde_json::{Map, Value};
use tracing::field::{Field, Visit};
use tracing::{Event, Level, Subscriber};
use tracing_subscriber::fmt::format::{JsonFields, Writer};
use tracing_subscriber::fmt::{FmtContext, FormatEvent, FormatFields, FormattedFields, MakeWriter};
use tracing_subscriber::registry::LookupSpan;

pub struct FlatJson;

struct Fields<'a>(&'a mut Map<String, Value>);

impl Visit for Fields<'_> {
    fn record_str(&mut self, field: &Field, value: &str) {
        self.0.insert(field.name().into(), Value::from(value));
    }
    fn record_i64(&mut self, field: &Field, value: i64) {
        self.0.insert(field.name().into(), Value::from(value));
    }
    fn record_u64(&mut self, field: &Field, value: u64) {
        self.0.insert(field.name().into(), Value::from(value));
    }
    fn record_bool(&mut self, field: &Field, value: bool) {
        self.0.insert(field.name().into(), Value::from(value));
    }
    fn record_f64(&mut self, field: &Field, value: f64) {
        self.0.insert(field.name().into(), Value::from(value));
    }
    fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
        self.0
            .insert(field.name().into(), Value::from(format!("{value:?}")));
    }
}

impl<S, N> FormatEvent<S, N> for FlatJson
where
    S: Subscriber + for<'a> LookupSpan<'a>,
    N: for<'a> FormatFields<'a> + 'static,
{
    fn format_event(
        &self,
        ctx: &FmtContext<'_, S, N>,
        mut writer: Writer<'_>,
        event: &Event<'_>,
    ) -> fmt::Result {
        let mut map = Map::new();
        let now = jiff::Timestamp::now();
        map.insert(
            "timestamp".into(),
            Value::from(now.strftime("%Y-%m-%dT%H:%M:%S%.3fZ").to_string()),
        );
        map.insert(
            "level".into(),
            Value::from(event.metadata().level().as_str()),
        );
        map.insert("target".into(), Value::from(event.metadata().target()));
        if let Some(scope) = ctx.event_scope() {
            let mut names = Vec::new();
            for span in scope.from_root() {
                names.push(span.name());
                if let Some(f) = span.extensions().get::<FormattedFields<N>>()
                    && let Ok(Value::Object(fields)) = serde_json::from_str::<Value>(&f.fields)
                {
                    map.extend(fields);
                }
            }
            map.insert("span".into(), Value::from(names.join(":")));
        }
        event.record(&mut Fields(&mut map));
        writeln!(writer, "{}", Value::Object(map))
    }
}

/// `SAKA_LOG` = `error | warn | info | debug | trace` (default `info`).
fn level() -> Level {
    std::env::var("SAKA_LOG")
        .ok()
        .and_then(|l| l.parse().ok())
        .unwrap_or(Level::INFO)
}

pub fn init() {
    init_with(std::io::stderr);
}

pub fn init_with<W>(writer: W)
where
    W: for<'w> MakeWriter<'w> + Send + Sync + 'static,
{
    let _ = tracing_subscriber::fmt()
        .fmt_fields(JsonFields::new())
        .event_format(FlatJson)
        .with_max_level(level())
        .with_writer(writer)
        .try_init();
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::*;

    #[derive(Clone, Default)]
    struct Buffer(Arc<Mutex<Vec<u8>>>);

    impl std::io::Write for Buffer {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn lines_inside_a_span_carry_its_fields_at_the_top_level() {
        let buffer = Buffer::default();
        let out = buffer.clone();
        let subscriber = tracing_subscriber::fmt()
            .fmt_fields(JsonFields::new())
            .event_format(FlatJson)
            .with_writer(move || out.clone())
            .finish();
        tracing::subscriber::with_default(subscriber, || {
            let span = tracing::info_span!(
                "job",
                correlation_id = "c-1",
                job_kind = "scm.merge",
                route = tracing::field::Empty
            );
            let _in = span.enter();
            span.record("route", "/x");
            tracing::info!(attempt = 2, "running");
            let inner = tracing::info_span!("call", work_item_id = "w-1");
            inner.in_scope(|| tracing::warn!(error = %"boom", "failed"));
        });
        let text = String::from_utf8(buffer.0.lock().unwrap().clone()).unwrap();
        let lines: Vec<Value> = text
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert_eq!(lines.len(), 2);
        for line in &lines {
            assert_eq!(line["correlation_id"], "c-1");
            assert_eq!(line["job_kind"], "scm.merge");
            assert!(line["timestamp"].as_str().unwrap().ends_with('Z'));
        }
        assert_eq!(
            (
                &lines[0]["message"],
                &lines[0]["attempt"],
                &lines[0]["route"]
            ),
            (&Value::from("running"), &Value::from(2), &Value::from("/x"))
        );
        assert_eq!(
            (
                &lines[1]["work_item_id"],
                &lines[1]["error"],
                &lines[1]["span"]
            ),
            (
                &Value::from("w-1"),
                &Value::from("boom"),
                &Value::from("job:call")
            )
        );
    }
}
