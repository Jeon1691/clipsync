use serde::{Deserialize, Serialize};
use ulid::Ulid;

macro_rules! ulid_id {
    ($name:ident) => {
        #[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(pub String);

        impl $name {
            pub fn new() -> Self {
                Self(Ulid::new().to_string())
            }

            pub fn as_str(&self) -> &str {
                &self.0
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

        impl From<String> for $name {
            fn from(value: String) -> Self {
                Self(value)
            }
        }

        impl From<&str> for $name {
            fn from(value: &str) -> Self {
                Self(value.to_string())
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
