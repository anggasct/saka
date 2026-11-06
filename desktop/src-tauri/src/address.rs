use url::{Host, Url};

use crate::error::ShellError;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerAddress(String);

impl ServerAddress {
    pub fn parse(input: &str) -> Result<ServerAddress, ShellError> {
        let url = Url::parse(input.trim()).map_err(|_| ShellError::InvalidUrl)?;
        let has_extras = !url.username().is_empty()
            || url.password().is_some()
            || !matches!(url.path(), "" | "/")
            || url.query().is_some()
            || url.fragment().is_some();
        let host = url.host().ok_or(ShellError::InvalidUrl)?;
        if has_extras {
            return Err(ShellError::InvalidUrl);
        }
        match url.scheme() {
            "https" => {}
            "http" if loopback(&host) => {}
            "http" => return Err(ShellError::InsecureUrl),
            _ => return Err(ShellError::InvalidUrl),
        }
        Ok(ServerAddress(url.origin().ascii_serialization()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn endpoint(&self, path: &str) -> String {
        format!("{}{path}", self.0)
    }
}

fn loopback(host: &Host<&str>) -> bool {
    match host {
        Host::Domain(name) => name.eq_ignore_ascii_case("localhost"),
        Host::Ipv4(ip) => ip.is_loopback(),
        Host::Ipv6(ip) => ip.is_loopback(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn https_and_loopback_http_are_accepted_and_normalised_to_an_origin() {
        for (input, origin) in [
            ("https://saka.example.com", "https://saka.example.com"),
            ("  https://saka.example.com/ ", "https://saka.example.com"),
            (
                "HTTPS://Saka.Example.com:8443",
                "https://saka.example.com:8443",
            ),
            ("http://127.0.0.1:7420", "http://127.0.0.1:7420"),
            ("http://localhost:7420/", "http://localhost:7420"),
            ("http://LOCALHOST", "http://localhost"),
            ("http://[::1]:7420", "http://[::1]:7420"),
            ("http://127.1.2.3:80", "http://127.1.2.3"),
        ] {
            let parsed = ServerAddress::parse(input);
            assert_eq!(
                parsed.as_ref().map(ServerAddress::as_str),
                Ok(origin),
                "{input}"
            );
        }
    }

    #[test]
    fn plain_http_to_anything_but_loopback_is_refused_as_insecure() {
        for input in [
            "http://saka.example.com",
            "http://10.0.0.5:7420",
            "http://192.168.1.2",
            "http://localhost.example.com",
            "http://[2001:db8::1]",
            "http://0.0.0.0:7420",
        ] {
            assert_eq!(
                ServerAddress::parse(input),
                Err(ShellError::InsecureUrl),
                "{input}"
            );
        }
    }

    #[test]
    fn anything_that_is_not_a_bare_origin_is_invalid() {
        for input in [
            "",
            "saka.example.com",
            "ftp://saka.example.com",
            "file:///etc/passwd",
            "https://user:pass@saka.example.com",
            "https://saka.example.com/api",
            "https://saka.example.com/?x=1",
            "https://saka.example.com/#top",
            "tauri://localhost",
            "javascript:alert(1)",
        ] {
            assert_eq!(
                ServerAddress::parse(input),
                Err(ShellError::InvalidUrl),
                "{input}"
            );
        }
    }

    #[test]
    fn an_endpoint_joins_the_origin_and_a_path() {
        let address = ServerAddress::parse("https://saka.example.com").unwrap();
        assert_eq!(
            address.endpoint("/api/v1/auth/pair"),
            "https://saka.example.com/api/v1/auth/pair"
        );
    }
}
