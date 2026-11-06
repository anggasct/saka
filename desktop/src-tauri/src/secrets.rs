use std::fmt;

use zeroize::Zeroizing;

use crate::error::ShellError;

pub const SERVICE: &str = "dev.saka.desktop";

pub struct Token(Zeroizing<String>);

impl Token {
    pub fn new(value: String) -> Token {
        Token(Zeroizing::new(value))
    }

    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Token {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Token(..)")
    }
}

pub trait SecretStore: Send + Sync {
    fn save(&self, id: &str, token: &Token) -> Result<(), ShellError>;
    fn load(&self, id: &str) -> Result<Option<Token>, ShellError>;
    fn remove(&self, id: &str) -> Result<(), ShellError>;
}

pub struct Keychain;

fn entry(id: &str) -> Result<keyring::Entry, ShellError> {
    keyring::Entry::new(SERVICE, id).map_err(|e| {
        tracing::warn!(error = %e, "keychain entry unavailable");
        ShellError::KeychainUnavailable
    })
}

fn unavailable(error: keyring::Error) -> ShellError {
    tracing::warn!(error = %error, "keychain call failed");
    ShellError::KeychainUnavailable
}

impl SecretStore for Keychain {
    fn save(&self, id: &str, token: &Token) -> Result<(), ShellError> {
        entry(id)?.set_password(token.expose()).map_err(unavailable)
    }

    fn load(&self, id: &str) -> Result<Option<Token>, ShellError> {
        match entry(id)?.get_password() {
            Ok(value) => Ok(Some(Token::new(value))),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(unavailable(e)),
        }
    }

    fn remove(&self, id: &str) -> Result<(), ShellError> {
        match entry(id)?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(unavailable(e)),
        }
    }
}

#[cfg(test)]
pub mod memory {
    use std::collections::HashMap;
    use std::sync::Mutex;

    use super::*;

    #[derive(Default)]
    pub struct Memory {
        entries: Mutex<HashMap<String, String>>,
        pub broken: Mutex<bool>,
    }

    impl Memory {
        pub fn ids(&self) -> Vec<String> {
            let mut ids: Vec<String> = self.entries.lock().unwrap().keys().cloned().collect();
            ids.sort();
            ids
        }

        pub fn value(&self, id: &str) -> Option<String> {
            self.entries.lock().unwrap().get(id).cloned()
        }

        fn check(&self) -> Result<(), ShellError> {
            if *self.broken.lock().unwrap() {
                Err(ShellError::KeychainUnavailable)
            } else {
                Ok(())
            }
        }
    }

    impl SecretStore for Memory {
        fn save(&self, id: &str, token: &Token) -> Result<(), ShellError> {
            self.check()?;
            self.entries
                .lock()
                .unwrap()
                .insert(id.to_string(), token.expose().to_string());
            Ok(())
        }

        fn load(&self, id: &str) -> Result<Option<Token>, ShellError> {
            self.check()?;
            Ok(self
                .entries
                .lock()
                .unwrap()
                .get(id)
                .map(|v| Token::new(v.clone())))
        }

        fn remove(&self, id: &str) -> Result<(), ShellError> {
            self.check()?;
            self.entries.lock().unwrap().remove(id);
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_token_never_prints_its_value() {
        let token = Token::new("sdt_super_secret".into());
        assert_eq!(format!("{token:?}"), "Token(..)");
        assert_eq!(token.expose(), "sdt_super_secret");
    }
}
