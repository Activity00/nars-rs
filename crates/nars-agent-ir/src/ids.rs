//! Stable identifiers used by the agent protocol.
//!
//! Identifiers are opaque strings with a human-readable prefix. They are
//! serialized transparently so the wire format is `"run_..."` rather than an
//! object wrapper.

use std::fmt;

use serde::{Deserialize, Serialize};

macro_rules! identifier {
    ($name:ident, $prefix:literal) => {
        #[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(String);

        impl $name {
            /// Generates a fresh random identifier.
            #[must_use]
            pub fn new() -> Self {
                Self(format!("{}_{}", $prefix, uuid::Uuid::new_v4().simple()))
            }

            /// Builds an identifier from a known value, mainly for tests and replay.
            #[must_use]
            pub fn from_static(value: impl Into<String>) -> Self {
                Self(value.into())
            }

            /// Borrows the identifier as a string slice.
            #[must_use]
            pub fn as_str(&self) -> &str {
                &self.0
            }

            /// Returns the suffix after the first `_`, suitable for a Narsese atom.
            #[must_use]
            pub fn short(&self) -> &str {
                self.0
                    .split_once('_')
                    .map_or(self.0.as_str(), |(_, tail)| tail)
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str(&self.0)
            }
        }

        impl From<String> for $name {
            fn from(value: String) -> Self {
                Self(value)
            }
        }

        impl From<&str> for $name {
            fn from(value: &str) -> Self {
                Self(value.to_owned())
            }
        }

        impl AsRef<str> for $name {
            fn as_ref(&self) -> &str {
                &self.0
            }
        }
    };
}

identifier!(SessionId, "ses");
identifier!(RunId, "run");
identifier!(ToolCallId, "call");
identifier!(EventId, "evt");
identifier!(ObservationId, "obs");
identifier!(RequestId, "req");

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identifiers_round_trip_transparently() {
        let id = RunId::from_static("run_demo");
        assert_eq!(id.as_str(), "run_demo");
        assert_eq!(id.short(), "demo");
        let json = serde_json::to_string(&id).unwrap();
        assert_eq!(json, r#""run_demo""#);
        let decoded: RunId = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded, id);
    }

    #[test]
    fn generated_identifiers_have_the_prefix() {
        let id = SessionId::new();
        assert!(id.as_str().starts_with("ses_"));
        assert_ne!(id, SessionId::new());
    }
}
