//! The owner token: `nymo_` and 32 random bytes, kept in a 0600 file.

use std::io::Write;
use std::path::Path;

use rand::RngCore;
use subtle::ConstantTimeEq;

pub const PREFIX: &str = "nymo_";

#[derive(Debug, thiserror::Error)]
pub enum TokenError {
    #[error("token file io: {0}")]
    Io(#[from] std::io::Error),
    #[error("the token file is readable by others (mode {0:o}); fix it or delete it")]
    Mode(u32),
    #[error("the token file does not hold an owner token")]
    Malformed,
}

pub struct OwnerToken(String);

impl OwnerToken {
    /// Reads the token at `path`, or writes a new one with mode 0600 on first start.
    pub fn load_or_create(path: &Path) -> Result<Self, TokenError> {
        match std::fs::read_to_string(path) {
            Ok(text) => {
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    let mode = std::fs::metadata(path)?.permissions().mode() & 0o777;
                    if mode & 0o077 != 0 {
                        return Err(TokenError::Mode(mode));
                    }
                }
                let text = text.trim().to_string();
                let body = text.strip_prefix(PREFIX).ok_or(TokenError::Malformed)?;
                if body.len() != 64 || !body.bytes().all(|b| b.is_ascii_hexdigit()) {
                    return Err(TokenError::Malformed);
                }
                Ok(Self(text))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let mut bytes = [0u8; 32];
                rand::rngs::OsRng.fill_bytes(&mut bytes);
                let token = format!("{PREFIX}{}", hex::encode(bytes));
                let mut options = std::fs::OpenOptions::new();
                options.write(true).create_new(true);
                #[cfg(unix)]
                std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
                let mut file = options.open(path)?;
                file.write_all(token.as_bytes())?;
                file.sync_all()?;
                Ok(Self(token))
            }
            Err(e) => Err(e.into()),
        }
    }

    pub fn matches(&self, presented: &str) -> bool {
        self.0.as_bytes().ct_eq(presented.as_bytes()).into()
    }
}

impl std::fmt::Debug for OwnerToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("OwnerToken(..)")
    }
}
