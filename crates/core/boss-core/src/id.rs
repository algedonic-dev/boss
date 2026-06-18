use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Newtype wrapper for type-safe identifiers.
/// Each domain entity gets its own ID type via the `define_id!` macro.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Id(Uuid);

impl Id {
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }

    pub fn from_uuid(uuid: Uuid) -> Self {
        Self(uuid)
    }

    pub fn as_uuid(&self) -> &Uuid {
        &self.0
    }
}

impl Default for Id {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Display for Id {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Define a strongly-typed domain ID.
///
/// ```
/// use boss_core::define_id;
/// define_id!(OrderId);
/// define_id!(DeviceId);
/// ```
#[macro_export]
macro_rules! define_id {
    ($name:ident) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
        pub struct $name($crate::id::Id);

        impl $name {
            pub fn new() -> Self {
                Self($crate::id::Id::new())
            }

            pub fn from_uuid(uuid: uuid::Uuid) -> Self {
                Self($crate::id::Id::from_uuid(uuid))
            }

            pub fn inner(&self) -> &$crate::id::Id {
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
                write!(f, "{}", self.0)
            }
        }
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_unique() {
        let a = Id::new();
        let b = Id::new();
        assert_ne!(a, b);
    }

    #[test]
    fn id_display_matches_uuid() {
        let id = Id::new();
        assert_eq!(id.to_string(), id.as_uuid().to_string());
    }

    #[test]
    fn id_serde_round_trip() {
        let id = Id::new();
        let json = serde_json::to_string(&id).unwrap();
        let back: Id = serde_json::from_str(&json).unwrap();
        assert_eq!(id, back);
    }

    #[test]
    fn id_default_is_not_nil() {
        let id = Id::default();
        assert!(!id.as_uuid().is_nil());
    }
}
