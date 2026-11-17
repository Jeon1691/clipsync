// Copyright the ClipSync contributors.
// SPDX-License-Identifier: MIT
//! Wire identifiers.
//!
//! IDs arrive from the relay and from other devices, become map keys, are
//! shown to the user and forwarded to webhooks, and form parts of file
//! names (for example `room-{room_id}.secret`). Every ID decoded from the
//! wire or from a stored file is therefore checked by [`validate_id`]: a
//! valid ID is a short, printable, single file-name component on every
//! platform.
//!
//! The `From<String>` and `From<&str>` conversions, like building the
//! public tuple field directly, do not check: they are for values the
//! program builds itself (generated IDs, placeholders, test
//! fixtures). Text from any other source goes through [`str::parse`]
//! ([`std::str::FromStr`]), and code that turns an ID into a path calls
//! `validate` again in case the ID was built unchecked.
use serde::{Deserialize, Deserializer, Serialize};
use ulid::Ulid;

/// Longest identifier accepted, in bytes. Generated IDs are 26-character
/// ULIDs; the bound leaves room for older and test IDs while keeping file
/// names and printed lines short. It matches the bound the client applies to
/// relay-supplied room and device IDs before printing them.
pub const ID_MAX_LEN: usize = 64;

/// Separator allowed between the other characters of an ID, as in
/// `name.v1`. It may not start or end an ID or appear twice in a row, so no
/// ID is `.`, `..`, a hidden file name, or a name Windows silently shortens.
const ID_DOT: u8 = b'.';

/// Why a string is not a valid identifier. The message never repeats the
/// rejected text, which may hold control characters.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum IdError {
    #[error("identifier is empty")]
    Empty,
    #[error("identifier is longer than {ID_MAX_LEN} bytes")]
    TooLong,
    #[error("identifier may only contain ASCII letters, digits, '-', '_' and '.'")]
    Character,
    #[error("identifier may not start or end with '.' or contain '..'")]
    Dot,
}

/// Check that `id` is a valid identifier: 1 to [`ID_MAX_LEN`] bytes of ASCII
/// letters, digits, `-`, `_` and `.`, where `.` only separates other
/// characters.
pub fn validate_id(id: &str) -> Result<(), IdError> {
    let bytes = id.as_bytes();
    if bytes.is_empty() {
        return Err(IdError::Empty);
    }
    if bytes.len() > ID_MAX_LEN {
        return Err(IdError::TooLong);
    }
    if !bytes
        .iter()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(*byte, b'-' | b'_' | ID_DOT))
    {
        return Err(IdError::Character);
    }
    let doubled = bytes.windows(2).any(|pair| pair == [ID_DOT, ID_DOT]);
    if bytes.first() == Some(&ID_DOT) || bytes.last() == Some(&ID_DOT) || doubled {
        return Err(IdError::Dot);
    }
    Ok(())
}

macro_rules! ulid_id {
    ($name:ident) => {
        #[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize)]
        #[serde(transparent)]
        pub struct $name(pub String);

        impl $name {
            pub fn new() -> Self {
                Self(Ulid::generate().to_string())
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }

            /// Check this ID again, for code about to use it in a path or
            /// another place where an unchecked `From` value must not reach.
            pub fn validate(&self) -> Result<(), IdError> {
                validate_id(&self.0)
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(&self.0)
            }
        }

        /// Unchecked; see the module documentation.
        impl From<String> for $name {
            fn from(value: String) -> Self {
                Self(value)
            }
        }

        /// Unchecked; see the module documentation.
        impl From<&str> for $name {
            fn from(value: &str) -> Self {
                Self(value.to_string())
            }
        }

        impl std::str::FromStr for $name {
            type Err = IdError;

            fn from_str(value: &str) -> Result<Self, IdError> {
                validate_id(value)?;
                Ok(Self(value.to_string()))
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                let value = String::deserialize(deserializer)?;
                validate_id(&value).map_err(serde::de::Error::custom)?;
                Ok(Self(value))
            }
        }
    };
}

ulid_id!(DeviceId);
ulid_id!(RoomId);
ulid_id!(MessageId);
ulid_id!(TransferId);
ulid_id!(PairingSessionId);

impl TransferId {
    pub fn to_bytes(&self) -> [u8; 16] {
        Ulid::from_string(&self.0)
            .map(|u| u.0.to_be_bytes())
            .unwrap_or_else(|_| {
                let mut out = [0u8; 16];
                let bytes = self.0.as_bytes();
                let n = bytes.len().min(16);
                out[..n].copy_from_slice(&bytes[..n]);
                out
            })
    }

    pub fn from_bytes(bytes: [u8; 16]) -> Self {
        let ulid = Ulid(u128::from_be_bytes(bytes));
        Self(ulid.to_string())
    }
}
