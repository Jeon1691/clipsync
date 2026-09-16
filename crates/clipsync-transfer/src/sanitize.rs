use std::path::{Component, Path};

use crate::{Result, TransferError};

pub fn safe_filename(name: &str) -> Result<String> {
    if name.is_empty() || name.contains('\0') || name.contains('/') || name.contains('\\') {
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
            if s == "." || s == ".." || s.is_empty() {
                return Err(TransferError::UnsafeName(name.into()));
            }
            Ok(s.to_string())
        }
        _ => Err(TransferError::UnsafeName(name.into())),
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
    }
}
