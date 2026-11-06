use serde::Serialize;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ShellError {
    #[error("the address is not a valid server address")]
    InvalidUrl,
    #[error("plain http is only accepted for a loopback address")]
    InsecureUrl,
    #[error("the connection name must be 1 to 100 characters")]
    InvalidName,
    #[error("the pairing code was refused")]
    PairingCodeInvalid,
    #[error("too many attempts")]
    RateLimited,
    #[error("the server does not use pairing")]
    PairingUnavailable,
    #[error("the server could not be reached")]
    ServerUnreachable,
    #[error("the server refused the request")]
    ServerRefused,
    #[error("the OS keychain is not available")]
    KeychainUnavailable,
    #[error("no such connection")]
    NotFound,
    #[error("the connection list could not be saved")]
    StoreFailed,
}

impl ShellError {
    pub fn code(&self) -> &'static str {
        match self {
            ShellError::InvalidUrl => "invalid_url",
            ShellError::InsecureUrl => "insecure_url",
            ShellError::InvalidName => "invalid_name",
            ShellError::PairingCodeInvalid => "pairing_code_invalid",
            ShellError::RateLimited => "rate_limited",
            ShellError::PairingUnavailable => "pairing_unavailable",
            ShellError::ServerUnreachable => "server_unreachable",
            ShellError::ServerRefused => "server_refused",
            ShellError::KeychainUnavailable => "keychain_unavailable",
            ShellError::NotFound => "not_found",
            ShellError::StoreFailed => "store_failed",
        }
    }
}

#[derive(Serialize)]
struct Body {
    code: &'static str,
}

impl Serialize for ShellError {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        Body { code: self.code() }.serialize(serializer)
    }
}
