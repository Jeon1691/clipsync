use std::path::{Component, Path};

use crate::{Result, TransferError};

pub fn safe_filename(name: &str) -> Result<String> {
    if name.is_empty()
        || name.contains('\0')
        || name.contains('/')
        || name.contains('\\')
        || name.contains(':')
        || name.chars().any(|c| c.is_control() || matches!(c, '*' | '?' | '"' | '<' | '>' | '|'))
    {
        return Err(TransferError::UnsafeName(name.into()));
    }
    let path = Path::new(name);
    if path.is_absolute() {
        return Err(TransferError::UnsafeName(name.into()));
    }
    let mut comps = path.components();
    match comps.next() {
        Some(Component::Normal(os)) => {
            if comps.next().is_some() {
                return Err(TransferError::UnsafeName(name.into()));
            }
            let s = os
                .to_str()
                .ok_or_else(|| TransferError::UnsafeName(name.into()))?;
            if s == "." || s == ".." || s.is_empty() || s.ends_with('.') || s.ends_with(' ') {
                return Err(TransferError::UnsafeName(name.into()));
            }
            if is_windows_device(s) {
                return Err(TransferError::UnsafeName(name.into()));
            }
            Ok(s.to_string())
        }
        _ => Err(TransferError::UnsafeName(name.into())),
    }
}

fn is_windows_device(name: &str) -> bool {
    let stem = name.split('.').next().unwrap_or(name);
    let upper = stem.to_ascii_uppercase();
    matches!(
        upper.as_str(),
        "CON" | "PRN" | "AUX" | "NUL" | "COM1" | "COM2" | "COM3" | "COM4" | "COM5" | "COM6"
            | "COM7" | "COM8" | "COM9" | "LPT1" | "LPT2" | "LPT3" | "LPT4" | "LPT5" | "LPT6"
            | "LPT7" | "LPT8" | "LPT9"
    )
}

/// Staging directories are named with the transfer id from the peer.
/// Only a Crockford ULID is allowed, so `../` cannot escape the inbox.
pub fn safe_transfer_id(id: &str) -> Result<String> {
    if id.len() == 26
        && id.chars().all(|c| {
            matches!(c,
                '0'..='9' | 'A'..='H' | 'J'..='N' | 'P'..='T' | 'V'..='Z'
            )
        })
    {
        Ok(id.to_string())
    } else {
        Err(TransferError::UnsafeName(id.into()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_basename() {
        assert_eq!(safe_filename("report.pdf").unwrap(), "report.pdf");
    }

    #[test]
    fn rejects_traversal() {
        assert!(safe_filename("../etc/passwd").is_err());
        assert!(safe_filename("/tmp/x").is_err());
        assert!(safe_filename("a/b").is_err());
        assert!(safe_filename("..").is_err());
        assert!(safe_filename("a\0b").is_err());
        assert!(safe_filename("secret.txt:ads").is_err());
        assert!(safe_filename("CON").is_err());
        assert!(safe_filename("aux.txt").is_err());
        assert!(safe_filename("file.").is_err());
    }

    #[test]
    fn transfer_id_must_be_ulid() {
        assert!(safe_transfer_id("01ARZ3NDEKTSV4RRFFQ69G5FAV").is_ok());
        assert!(safe_transfer_id("../etc").is_err());
        assert!(safe_transfer_id("01ARZ3NDEKTSV4RRFFQ69G5FA").is_err());
        assert!(safe_transfer_id("01arz3ndektsv4rrffq69g5fav").is_err());
    }
}
