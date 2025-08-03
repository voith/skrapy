use erased_serde::Serialize as ErasedSerialize;
use std::any::Any;

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

/// Minimal placeholder item (kept for backward-compatibility in tests).
#[derive(Default, serde::Serialize)]
pub struct Item {}
