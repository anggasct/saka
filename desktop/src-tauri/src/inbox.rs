use serde::Deserialize;

use crate::error::ShellError;
use crate::labels::Labels;

const SUMMARY_PATH: &str = "/api/v1/inbox/summary";
const NEWEST_PATH: &str = "/api/v1/inbox?state=open&limit=1";
const INBOX_ROUTE: &str = "/";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notice {
    pub title: String,
    pub body: String,
    pub route: String,
}

#[derive(Deserialize)]
struct Summary {
    open: i64,
}

#[derive(Deserialize)]
struct Page {
    items: Vec<Item>,
}

#[derive(Deserialize)]
struct Item {
    id: String,
}

pub fn item_route(id: &str) -> String {
    format!("/inbox/{id}")
}

pub fn safe_route(route: &str) -> String {
    let plain = route.starts_with('/')
        && !route.starts_with("//")
        && !route.contains("://")
        && !route.contains('\\')
        && !route.chars().any(char::is_control);
    if plain {
        route.to_string()
    } else {
        INBOX_ROUTE.to_string()
    }
}

pub fn decide(
    previous: Option<i64>,
    current: i64,
    newest: Option<&str>,
    labels: Labels,
) -> Option<Notice> {
    let previous = previous?;
    if current <= previous {
        return None;
    }
    let route = match (current - previous, newest) {
        (1, Some(id)) => item_route(id),
        _ => INBOX_ROUTE.to_string(),
    };
    Some(Notice {
        title: "Saka".to_string(),
        body: labels.waiting(current),
        route,
    })
}

async fn get(
    http: &reqwest::Client,
    base: &str,
    path: &str,
    token: Option<&str>,
) -> Result<reqwest::Response, ShellError> {
    let mut request = http.get(format!("{base}{path}"));
    if let Some(token) = token {
        request = request.bearer_auth(token);
    }
    let response = request.send().await.map_err(|e| {
        tracing::debug!(error = %e.without_url(), "inbox poll failed");
        ShellError::ServerUnreachable
    })?;
    if response.status().is_success() {
        Ok(response)
    } else {
        Err(ShellError::ServerRefused)
    }
}

pub async fn open_count(
    http: &reqwest::Client,
    base: &str,
    token: Option<&str>,
) -> Result<i64, ShellError> {
    let summary: Summary = get(http, base, SUMMARY_PATH, token)
        .await?
        .json()
        .await
        .map_err(|_| ShellError::ServerRefused)?;
    Ok(summary.open)
}

pub async fn newest_item(
    http: &reqwest::Client,
    base: &str,
    token: Option<&str>,
) -> Option<String> {
    let page: Page = get(http, base, NEWEST_PATH, token)
        .await
        .ok()?
        .json()
        .await
        .ok()?;
    page.items.into_iter().next().map(|item| item.id)
}

#[derive(Default)]
pub struct Watcher {
    previous: Option<i64>,
}

pub struct Tick {
    pub open: i64,
    pub notice: Option<Notice>,
}

impl Watcher {
    pub fn reset(&mut self) {
        self.previous = None;
    }

    pub async fn tick(
        &mut self,
        http: &reqwest::Client,
        base: &str,
        token: Option<&str>,
        labels: Labels,
    ) -> Result<Tick, ShellError> {
        let open = open_count(http, base, token).await?;
        let rose = self.previous.is_some_and(|before| open > before);
        let newest = if rose {
            newest_item(http, base, token).await
        } else {
            None
        };
        let notice = decide(self.previous, open, newest.as_deref(), labels);
        self.previous = Some(open);
        Ok(Tick { open, notice })
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use wiremock::matchers::{header, method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::*;

    #[test]
    fn a_notice_appears_only_when_the_open_count_rises() {
        let en = Labels::En;
        assert_eq!(decide(None, 4, Some("x"), en), None);
        assert_eq!(decide(Some(2), 2, Some("x"), en), None);
        assert_eq!(decide(Some(3), 1, Some("x"), en), None);
        let one = decide(Some(1), 2, Some("abc"), en).unwrap();
        assert_eq!(one.route, "/inbox/abc");
        assert_eq!(one.body, "2 items are waiting for you");
        assert_eq!(one.title, "Saka");
    }

    #[test]
    fn several_new_items_or_an_unknown_newest_open_the_inbox_list() {
        assert_eq!(
            decide(Some(1), 4, Some("abc"), Labels::En).unwrap().route,
            "/"
        );
        assert_eq!(decide(Some(0), 1, None, Labels::En).unwrap().route, "/");
    }

    #[test]
    fn a_route_from_the_page_must_stay_inside_the_app() {
        for (input, expected) in [
            ("/inbox/abc", "/inbox/abc"),
            ("/delivery?state=blocked", "/delivery?state=blocked"),
            ("//evil.test", "/"),
            ("https://evil.test", "/"),
            ("javascript:alert(1)", "/"),
            ("/a\\b", "/"),
            ("", "/"),
        ] {
            assert_eq!(safe_route(input), expected, "{input}");
        }
    }

    async fn inbox(open: i64, newest: &str) -> MockServer {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path(SUMMARY_PATH))
            .and(header("authorization", "Bearer sdt_t"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "open": open })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v1/inbox"))
            .and(query_param("state", "open"))
            .and(query_param("limit", "1"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(json!({ "items": [{ "id": newest }] })),
            )
            .mount(&server)
            .await;
        server
    }

    #[tokio::test]
    async fn the_watcher_takes_a_baseline_then_notifies_on_a_rise_with_the_newest_item() {
        let http = crate::pairing::client().unwrap();
        let mut watcher = Watcher::default();
        let first = inbox(2, "old").await;
        let tick = watcher
            .tick(&http, &first.uri(), Some("sdt_t"), Labels::En)
            .await
            .unwrap();
        assert_eq!((tick.open, tick.notice), (2, None));
        let rose = inbox(3, "fresh").await;
        let tick = watcher
            .tick(&http, &rose.uri(), Some("sdt_t"), Labels::En)
            .await
            .unwrap();
        assert_eq!(tick.notice.unwrap().route, "/inbox/fresh");
        let same = watcher
            .tick(&http, &rose.uri(), Some("sdt_t"), Labels::En)
            .await
            .unwrap();
        assert_eq!(same.notice, None);
    }

    #[tokio::test]
    async fn a_failed_poll_keeps_the_baseline_and_an_unauthorised_one_is_refused() {
        let http = crate::pairing::client().unwrap();
        let mut watcher = Watcher::default();
        let first = inbox(1, "a").await;
        watcher
            .tick(&http, &first.uri(), Some("sdt_t"), Labels::En)
            .await
            .unwrap();
        let down = "http://127.0.0.1:9";
        assert_eq!(
            watcher
                .tick(&http, down, Some("sdt_t"), Labels::En)
                .await
                .err(),
            Some(ShellError::ServerUnreachable)
        );
        let refusing = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(401))
            .mount(&refusing)
            .await;
        assert_eq!(
            watcher
                .tick(&http, &refusing.uri(), Some("bad"), Labels::En)
                .await
                .err(),
            Some(ShellError::ServerRefused)
        );
        let more = inbox(2, "b").await;
        let tick = watcher
            .tick(&http, &more.uri(), Some("sdt_t"), Labels::En)
            .await
            .unwrap();
        assert_eq!(tick.notice.unwrap().route, "/inbox/b");
    }

    #[tokio::test]
    async fn the_local_server_is_polled_without_a_token() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path(SUMMARY_PATH))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "open": 0 })))
            .mount(&server)
            .await;
        let http = crate::pairing::client().unwrap();
        assert_eq!(open_count(&http, &server.uri(), None).await, Ok(0));
    }
}
