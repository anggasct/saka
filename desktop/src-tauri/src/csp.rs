const BASE: &str = "default-src 'self'; script-src 'self' 'wasm-unsafe-eval'; style-src 'self' 'unsafe-inline'; img-src 'self' data: blob:; font-src 'self' data:; object-src 'none'; base-uri 'none'; form-action 'none'; frame-ancestors 'none'";
const CONNECT_BASE: &str = "'self' ipc: http://ipc.localhost";

pub fn policy(active: Option<&str>) -> String {
    let connect = match active {
        Some(origin) => format!("{CONNECT_BASE} {origin}"),
        None => CONNECT_BASE.to_string(),
    };
    format!("{BASE}; connect-src {connect}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn connect_src(policy: &str) -> &str {
        policy
            .split("; ")
            .find_map(|d| d.strip_prefix("connect-src "))
            .unwrap()
    }

    #[test]
    fn connect_src_names_only_the_active_connection() {
        let policy = policy(Some("https://saka.example.com"));
        assert_eq!(
            connect_src(&policy),
            "'self' ipc: http://ipc.localhost https://saka.example.com"
        );
    }

    #[test]
    fn without_a_connection_the_webview_can_reach_no_network_host() {
        let policy = policy(None);
        assert_eq!(connect_src(&policy), "'self' ipc: http://ipc.localhost");
        assert!(!policy.contains("http://127"));
        assert!(!policy.contains("https:"));
    }

    #[test]
    fn scripts_and_frames_stay_closed() {
        let policy = policy(None);
        assert!(policy.contains("script-src 'self' 'wasm-unsafe-eval'"));
        assert!(policy.contains("object-src 'none'"));
        assert!(policy.contains("frame-ancestors 'none'"));
        assert!(!policy.contains("'unsafe-eval'"));
    }
}
