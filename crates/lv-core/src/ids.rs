//! Stable, globally unique identifiers (invariant 4).
//!
//! Every shared identity is a UUIDv7 wrapped in its own type. The text form
//! is `<prefix>_<uuid>`, and parsing checks the prefix, so an ID of one kind
//! is never silently accepted as another. Storage uses the raw 16 bytes.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use uuid::Uuid;

/// Why an ID's text form did not parse.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum IdParseError {
    #[error("expected an ID starting with `{expected}_`, got `{got}`")]
    WrongPrefix { expected: &'static str, got: String },
    #[error("invalid UUID in `{0}`")]
    BadUuid(String),
}

macro_rules! id_type {
    ($(#[$m:meta])* $name:ident = $prefix:literal) => {
        $(#[$m])*
        #[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $name(Uuid);

        impl $name {
            pub const PREFIX: &'static str = $prefix;

            /// A new, time-ordered ID.
            pub fn new() -> Self {
                Self(Uuid::now_v7())
            }

            pub const fn from_uuid(uuid: Uuid) -> Self {
                Self(uuid)
            }

            pub const fn as_uuid(&self) -> &Uuid {
                &self.0
            }

            pub const fn from_bytes(bytes: [u8; 16]) -> Self {
                Self(Uuid::from_bytes(bytes))
            }

            pub const fn as_bytes(&self) -> &[u8; 16] {
                self.0.as_bytes()
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "{}_{}", $prefix, self.0.hyphenated())
            }
        }

        impl FromStr for $name {
            type Err = IdParseError;

            fn from_str(s: &str) -> Result<Self, Self::Err> {
                let rest = s
                    .strip_prefix($prefix)
                    .and_then(|r| r.strip_prefix('_'))
                    .ok_or_else(|| IdParseError::WrongPrefix { expected: $prefix, got: s.to_owned() })?;
                Uuid::try_parse(rest)
                    .map(Self)
                    .map_err(|_| IdParseError::BadUuid(s.to_owned()))
            }
        }

        impl Serialize for $name {
            fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                serializer.collect_str(self)
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                let s = <std::borrow::Cow<'de, str>>::deserialize(deserializer)?;
                s.parse().map_err(serde::de::Error::custom)
            }
        }

        #[cfg(feature = "sqlite")]
        impl rusqlite::ToSql for $name {
            fn to_sql(&self) -> rusqlite::Result<rusqlite::types::ToSqlOutput<'_>> {
                Ok(rusqlite::types::ToSqlOutput::Borrowed(
                    rusqlite::types::ValueRef::Blob(self.as_bytes()),
                ))
            }
        }

        #[cfg(feature = "sqlite")]
        impl rusqlite::types::FromSql for $name {
            fn column_result(value: rusqlite::types::ValueRef<'_>) -> rusqlite::types::FromSqlResult<Self> {
                let bytes = <[u8; 16]>::column_result(value)?;
                Ok(Self::from_bytes(bytes))
            }
        }
    };
}

id_type!(/// A project: one corpus, one SQLite file.
    ProjectId = "proj");
id_type!(DocumentId = "doc");
id_type!(/// A block or addressable inline node.
    NodeId = "node");
id_type!(OpId = "op");
id_type!(ChangeSetId = "cs");
id_type!(PersonId = "person");
id_type!(AgentId = "agent");
id_type!(GrantId = "grant");

/// Position in a project's op log, assigned by the project's sequencer.
///
/// This is an order, not an identity: ops are identified by [`OpId`].
/// `Version(0)` is the empty project; the first accepted op is `Version(1)`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Version(pub u64);

impl Version {
    pub const EMPTY: Version = Version(0);

    pub const fn next(self) -> Version {
        Version(self.0 + 1)
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "v{}", self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_form_roundtrips_and_checks_prefix() {
        let id = NodeId::new();
        let text = id.to_string();
        assert!(text.starts_with("node_"), "{text}");
        assert_eq!(text.parse::<NodeId>(), Ok(id));

        let err = text.parse::<DocumentId>().unwrap_err();
        assert!(matches!(err, IdParseError::WrongPrefix { expected: "doc", .. }));
        // `nodex_...` must not be mistaken for `node_...`.
        assert!(format!("nodex_{}", id.as_uuid()).parse::<NodeId>().is_err());
        assert!(matches!("node_nope".parse::<NodeId>(), Err(IdParseError::BadUuid(_))));
    }

    #[test]
    fn ids_are_v7_and_time_ordered() {
        let a = OpId::new();
        let b = OpId::new();
        assert_eq!(a.as_uuid().get_version_num(), 7);
        assert!(a < b);
    }

    #[test]
    fn serde_uses_text_form() {
        let id = GrantId::new();
        let json = serde_json::to_string(&id).unwrap();
        assert_eq!(json, format!("\"{id}\""));
        assert_eq!(serde_json::from_str::<GrantId>(&json).unwrap(), id);
        assert!(serde_json::from_str::<PersonId>(&json).is_err());
    }
}
