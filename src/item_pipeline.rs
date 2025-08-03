use crate::items::ItemBox;
use anyhow::Error as AnyError;
use async_trait::async_trait;
use serde_json as json;
use std::path::PathBuf;
use thiserror::Error;
use tokio::fs::OpenOptions;
use tokio::io::AsyncWriteExt;
use tokio::sync::Mutex as AsyncMutex;

/// Errors that can occur during pipeline processing
#[derive(Debug, Error)]
pub enum PipelineError {
    #[error("drop: {0}")]
    Drop(String),
    #[error(transparent)]
    Other(#[from] AnyError),
}

/// Outcome of a pipeline stage
pub enum PipelineOutcome {
    /// Continue to the next pipeline with (possibly transformed) item
    Continue(ItemBox),
    /// Drop the item with a reason (akin to Scrapy's DropItem)
    Drop(String),
}

#[async_trait]
pub trait Pipeline: Send + Sync {
    /// Called once before items start flowing
    async fn open(&self) -> Result<(), PipelineError> {
        Ok(())
    }

    /// Process one item
    async fn process(&self, item: ItemBox) -> Result<PipelineOutcome, PipelineError>;

    /// Called once when shutting down
    async fn close(&self) -> Result<(), PipelineError> {
        Ok(())
    }
}

/// Runs items through a sequence of pipelines
pub struct PipelineManager {
    pipes: Vec<Box<dyn Pipeline>>,
}

impl PipelineManager {
    pub fn new(pipes: Vec<Box<dyn Pipeline>>) -> Self {
        Self { pipes }
    }

    pub async fn open(&self) -> Result<(), PipelineError> {
        for p in &self.pipes {
            p.open().await?;
        }
        Ok(())
    }

    /// Run the item through each pipeline in order.
    /// Returns Ok(Some(item)) if it survives all pipelines, or Ok(None) if dropped.
    pub async fn process(&self, mut item: ItemBox) -> Result<Option<ItemBox>, PipelineError> {
        for p in &self.pipes {
            match p.process(item).await? {
                PipelineOutcome::Continue(next) => {
                    item = next;
                }
                PipelineOutcome::Drop(_why) => {
                    return Ok(None);
                }
            }
        }
        Ok(Some(item))
    }

    pub async fn close(&self) -> Result<(), PipelineError> {
        for p in &self.pipes {
            p.close().await?;
        }
        Ok(())
    }
}

/// Helper: serialize an `ItemBox` into `serde_json::Value`
fn item_to_json_value(item: &ItemBox) -> Result<serde_json::Value, PipelineError> {
    erased_serde::serialize(&**item, serde_json::value::Serializer)
        .map_err(|e| PipelineError::Other(AnyError::from(e)))
}

/// Example pipeline: ensure certain fields are present (works with map/JSON items)
pub struct RequireFields {
    required: Vec<String>,
}

impl RequireFields {
    pub fn new<I, S>(fields: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self {
            required: fields.into_iter().map(Into::into).collect(),
        }
    }
}

#[async_trait]
impl Pipeline for RequireFields {
    async fn process(&self, item: ItemBox) -> Result<PipelineOutcome, PipelineError> {
        let v = item_to_json_value(&item)?;
        let obj = match v.as_object() {
            Some(o) => o,
            None => return Ok(PipelineOutcome::Drop("item is not an object".into())),
        };
        for f in &self.required {
            if !obj.contains_key(f) {
                return Ok(PipelineOutcome::Drop(format!("missing field: {f}")));
            }
        }
        Ok(PipelineOutcome::Continue(item))
    }
}

/// Example pipeline: export items as JSON Lines (one JSON object per line)
pub struct JsonLinesExporter {
    path: PathBuf,
    file: AsyncMutex<Option<tokio::fs::File>>, // opened in `open()`
}

impl JsonLinesExporter {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            file: AsyncMutex::new(None),
        }
    }
}

#[async_trait]
impl Pipeline for JsonLinesExporter {
    async fn open(&self) -> Result<(), PipelineError> {
        let file = OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(&self.path)
            .await
            .map_err(|e| PipelineError::Other(AnyError::from(e)))?;
        *self.file.lock().await = Some(file);
        Ok(())
    }

    async fn process(&self, item: ItemBox) -> Result<PipelineOutcome, PipelineError> {
        // Serialize to a buffer
        let mut buf = Vec::new();
        {
            let mut ser = json::Serializer::new(&mut buf);
            erased_serde::serialize(&*item, &mut ser)
                .map_err(|e| PipelineError::Other(AnyError::from(e)))?;
        }
        buf.push(b'\n');

        if let Some(file) = self.file.lock().await.as_mut() {
            file.write_all(&buf)
                .await
                .map_err(|e| PipelineError::Other(AnyError::from(e)))?;
        } else {
            return Err(PipelineError::Other(AnyError::msg(
                "JsonLinesExporter not opened",
            )));
        }

        // Exporters usually consume the item (nothing more to do downstream)
        Ok(PipelineOutcome::Drop("exported".into()))
    }

    async fn close(&self) -> Result<(), PipelineError> {
        *self.file.lock().await = None; // drop handle
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::items::ItemBox;
    use serde::Serialize;
    use serde_json::json;
    use std::path::PathBuf;
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    use std::time::{SystemTime, UNIX_EPOCH};
    use tokio::fs;

    fn unique_temp_path(name: &str) -> PathBuf {
        let ts = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let pid = std::process::id();
        std::env::temp_dir().join(format!("{}_{}_{}.jsonl", name, pid, ts))
    }

    #[tokio::test]
    async fn require_fields_passes_on_object() {
        let item: ItemBox = Box::new(json!({"title":"Widget","price": 9.99}));
        let pipe = RequireFields::new(["title", "price"]);
        match pipe.process(item).await.unwrap() {
            PipelineOutcome::Continue(_) => {}
            other => panic!(
                "expected Continue, got {}",
                std::any::type_name_of_val(&other)
            ),
        }
    }

    #[tokio::test]
    async fn require_fields_drops_when_missing() {
        let item: ItemBox = Box::new(json!({"title":"Widget"})); // no price
        let pipe = RequireFields::new(["title", "price"]);
        match pipe.process(item).await.unwrap() {
            PipelineOutcome::Drop(reason) => assert!(reason.contains("missing field")),
            _ => panic!("expected Drop"),
        }
    }

    #[tokio::test]
    async fn require_fields_drops_non_object() {
        let item: ItemBox = Box::new(json!("just a string"));
        let pipe = RequireFields::new(["anything"]);
        match pipe.process(item).await.unwrap() {
            PipelineOutcome::Drop(reason) => assert!(reason.contains("not an object")),
            _ => panic!("expected Drop for non-object"),
        }
    }

    #[derive(Serialize)]
    struct Product {
        title: String,
        price: f64,
    }

    #[tokio::test]
    async fn require_fields_handles_typed_struct() {
        let item: ItemBox = Box::new(Product {
            title: "Widget".into(),
            price: 9.99,
        });
        let pipe = RequireFields::new(["title", "price"]);
        match pipe.process(item).await.unwrap() {
            PipelineOutcome::Continue(_) => {}
            _ => panic!("typed struct should pass"),
        }
    }

    #[tokio::test]
    async fn jsonlines_exporter_writes_line() {
        let path = unique_temp_path("jsonl_export");
        let exporter = JsonLinesExporter::new(&path);
        exporter.open().await.unwrap();

        let item: ItemBox = Box::new(json!({"title":"Gadget","price":1.23}));
        match exporter.process(item).await.unwrap() {
            PipelineOutcome::Drop(reason) => assert_eq!(reason, "exported"),
            _ => panic!("exporter should Drop after writing"),
        }

        exporter.close().await.unwrap();

        let contents = fs::read_to_string(&path).await.unwrap();
        // Should contain exactly one line of JSON ending with newline
        assert!(contents.ends_with('\n'));
        let lines: Vec<&str> = contents.lines().collect();
        assert_eq!(lines.len(), 1);
        let v: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(v["title"], "Gadget");
        assert_eq!(v["price"], 1.23);
    }

    // A test pipeline that records whether it ran
    struct CountingPipeline {
        hits: Arc<AtomicUsize>,
    }
    #[async_trait]
    impl Pipeline for CountingPipeline {
        async fn process(&self, item: ItemBox) -> Result<PipelineOutcome, PipelineError> {
            self.hits.fetch_add(1, Ordering::SeqCst);
            Ok(PipelineOutcome::Continue(item))
        }
    }

    // A pipeline that always drops
    struct Dropper;
    #[async_trait]
    impl Pipeline for Dropper {
        async fn process(&self, _item: ItemBox) -> Result<PipelineOutcome, PipelineError> {
            Ok(PipelineOutcome::Drop("nope".into()))
        }
    }

    #[tokio::test]
    async fn manager_runs_pipelines_and_stops_after_drop() {
        let hits = Arc::new(AtomicUsize::new(0));
        let p1 = Box::new(CountingPipeline { hits: hits.clone() });
        let p2 = Box::new(Dropper);
        let p3 = Box::new(CountingPipeline { hits: hits.clone() });
        let mgr = PipelineManager::new(vec![p1, p2, p3]);

        mgr.open().await.unwrap();
        let item: ItemBox = Box::new(json!({"a":1}));
        let out = mgr.process(item).await.unwrap();
        mgr.close().await.unwrap();

        assert!(out.is_none(), "item should be dropped by Dropper");
        assert_eq!(
            hits.load(Ordering::SeqCst),
            1,
            "pipelines after Drop must not run"
        );
    }

    // A pipeline that transforms the item by adding a field
    struct AddField {
        key: &'static str,
        val: i32,
    }
    #[async_trait]
    impl Pipeline for AddField {
        async fn process(&self, item: ItemBox) -> Result<PipelineOutcome, PipelineError> {
            let mut v = item_to_json_value(&item)?;
            let obj = v
                .as_object_mut()
                .ok_or_else(|| PipelineError::Drop("not an object".into()))?;
            obj.insert(self.key.to_string(), json!(self.val));
            let new_item: ItemBox = Box::new(v);
            Ok(PipelineOutcome::Continue(new_item))
        }
    }

    #[tokio::test]
    async fn manager_applies_transforms_in_order() {
        let mgr = PipelineManager::new(vec![
            Box::new(AddField { key: "x", val: 1 }),
            Box::new(AddField { key: "y", val: 2 }),
        ]);
        mgr.open().await.unwrap();
        let item: ItemBox = Box::new(json!({"title":"T"}));
        let out = mgr.process(item).await.unwrap().unwrap();
        mgr.close().await.unwrap();

        let v = item_to_json_value(&out).unwrap();
        assert_eq!(v["x"], 1);
        assert_eq!(v["y"], 2);
        assert_eq!(v["title"], "T");
    }
}
