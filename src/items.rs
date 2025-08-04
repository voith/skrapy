use erased_serde::Serialize as ErasedSerialize;
use serde_json;
use std::any::Any;
use std::fmt;

/// Type-erased item trait that allows heterogeneous items to flow through
/// pipelines while still being serializable and sendable across threads.
pub trait ItemDyn: ErasedSerialize + Any + Send + Sync {
    /// Downcasting support for pipelines that care about specific item types.
    fn as_any(&self) -> &dyn Any;
}

impl<T> ItemDyn for T
where
    T: serde::Serialize + Any + Send + Sync + 'static,
{
    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// Boxed, type-erased item used throughout the engine/pipelines.
pub type ItemBox = Box<dyn ItemDyn>;

impl fmt::Display for ItemBox {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Serialize the item to JSON into a buffer, then write it
        let mut buf = Vec::new();
        {
            let mut serializer = serde_json::Serializer::new(&mut buf);
            erased_serde::serialize(&**self, &mut serializer).map_err(|_| fmt::Error)?;
        }
        let s = String::from_utf8(buf).map_err(|_| fmt::Error)?;
        f.write_str(&s)
    }
}

impl std::fmt::Debug for Box<dyn ItemDyn> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Delegate to Display, printing the JSON representation.
        write!(f, "{}", self)
    }
}

/// Minimal placeholder item (kept for backward-compatibility in tests).
#[derive(Default, serde::Serialize)]
pub struct Item {}
