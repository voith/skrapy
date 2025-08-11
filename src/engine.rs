use crate::downloader::DownloadManager;
use crate::logger;
use crate::request::Request;
use crate::response::Response;
use crate::scheduler::Scheduler;
use crate::scraper::Scraper;
use crate::spider::Spider;
use tokio::signal;
use tokio::sync::mpsc;
use tokio::time::Duration;
use tokio_util::sync::CancellationToken;

pub struct Engine {
    download_manager: DownloadManager,
    scraper: Scraper,
    scheduler: Scheduler,
    downloader_response_rx: mpsc::Receiver<Response>,
    downloader_retried_request_rx: mpsc::Receiver<Request>,
    spider_request_rx: mpsc::Receiver<Request>,
    shutdown: CancellationToken,
    log_level: log::LevelFilter,
}

impl Engine {
    const SCRAPER_QUEUE_SIZE: usize = 1000;
    const CONCURRENCY_LIMIT: usize = 8;
    const DOWNLOADER_OUTPUT_QUEUE: usize = Self::CONCURRENCY_LIMIT * 100;

    pub fn new(spider: Box<dyn Spider>, log_level: log::LevelFilter) -> Self {
        let (downloader_response_tx, downloader_response_rx) =
            mpsc::channel(Self::DOWNLOADER_OUTPUT_QUEUE);
        let (downloader_retried_request_tx, downloader_retried_request_rx) =
            mpsc::channel(Self::DOWNLOADER_OUTPUT_QUEUE);
        let (spider_request_tx, spider_request_rx) = mpsc::channel(Self::SCRAPER_QUEUE_SIZE);
        let scraper = Scraper::new(spider, spider_request_tx);
        let scheduler = Scheduler::new();
        let shutdown = CancellationToken::new();
        Self {
            download_manager: DownloadManager::new(
                Self::CONCURRENCY_LIMIT,
                downloader_response_tx,
                downloader_retried_request_tx,
            ),
            scraper,
            scheduler,
            downloader_response_rx,
            downloader_retried_request_rx,
            spider_request_rx,
            shutdown,
            log_level,
        }
    }

    fn shutdown_if_idle(&self) {
        if self.download_manager.is_idle()
            && self.scraper.is_idle()
            && !self.scheduler.has_pending_requests()
            && self.downloader_response_rx.is_empty()
            && self.spider_request_rx.is_empty()
            && self.downloader_retried_request_rx.is_empty()
        {
            log::info!("finished scraping...");
            self.shutdown.cancel();
        }
    }

    pub async fn run(&mut self) -> anyhow::Result<()> {
        logger::init_logger(self.log_level)?;
        self.scraper.start_spider().await;
        let mut idle_interval = tokio::time::interval(Duration::from_millis(100));
        loop {
            tokio::select! {
                // Shutdown on Ctrl+C (SIGINT)
                _ = signal::ctrl_c() => {
                    log::warn!("Received Ctrl+C, shutting down engine");
                    self.shutdown.cancel();
                },
                // Stop when Shutdown
                _ = self.shutdown.cancelled() => {
                    log::info!("stopping engine");
                    self.download_manager.stop().await;
                    self.scraper.stop().await;
                    break anyhow::Ok(())?;
                },
                Some(request) = self.spider_request_rx.recv() => {
                    self.scheduler.enqueue_request(request);
                },
                // fetch new requests
                Some(_) = self.scheduler.peek_next_request(), if !self.download_manager.needs_backoff() => {
                    if let Some(request) = self.scheduler.next_request() {
                        self.download_manager.enqueue_request(request).await;
                    }
                }
                Some(response) = self.downloader_response_rx.recv() => {
                    self.scraper.process_response(response).await;
                },
                Some(retried_request) = self.downloader_retried_request_rx.recv() => {
                        self.scheduler.enqueue_request(retried_request);
                }
                _ = idle_interval.tick() => {},
            }
            // shudown if idle
            self.shutdown_if_idle();
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::item_pipeline::JsonLinesExporter;
    use crate::items::{Item, ItemBox};
    use crate::response::Response;
    use crate::spider::SpiderSettings;
    use crate::spider::{CallbackReturn, Spider, SpiderOutput};
    use httpmock::Method::GET;
    use httpmock::MockServer;
    use serde_json::json;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    #[tokio::test]
    async fn test_engine_handles_one_request_and_shuts_down() {
        static FLAG: AtomicBool = AtomicBool::new(false);

        // A callback that flips our FLAG
        fn dummy_callback(_: Response) -> CallbackReturn {
            FLAG.store(true, Ordering::SeqCst);
            Box::new(std::iter::empty())
        }

        struct TestSpider {
            base_url: String,
        }

        impl Spider for TestSpider {
            fn start(&self) -> CallbackReturn {
                let request = Request::get(&self.base_url).with_callback(dummy_callback);
                Box::new(vec![SpiderOutput::Request(request)].into_iter())
            }
        }

        // Start a mock HTTP server
        let server = MockServer::start();
        let _m = server.mock(|when, then| {
            when.method(GET).path("/");
            then.status(200).body("mocked");
        });

        // Build engine with TestSpider pointing to mock server
        let spider = Box::new(TestSpider {
            base_url: server.url("/"),
        });
        let mut engine = Engine::new(spider, log::LevelFilter::Info);

        // Run the engine; it should process the one request and then shut itself down
        let _ = engine.run().await;

        // Verify that our callback was invoked
        assert!(FLAG.load(Ordering::SeqCst), "Callback was not triggered");
    }

    #[tokio::test]
    async fn test_engine_callback_with_multiple_requests() {
        // Start mock server for HTTP requests
        let server = MockServer::start();
        let _m = server.mock(|when, then| {
            when.method(GET);
            then.status(200).body("ok");
        });
        let server_url = server.base_url();

        static CALLBACK_COUNT: AtomicUsize = AtomicUsize::new(0);

        // Callback that increments the coun
        fn counting_callback(_: Response) -> CallbackReturn {
            CALLBACK_COUNT.fetch_add(1, Ordering::SeqCst);
            let item: ItemBox = Box::new(Item::default());
            Box::new(std::iter::once(SpiderOutput::Item(item)))
        }

        struct BulkSpider {
            base_url: String,
        }
        impl Spider for BulkSpider {
            fn start(&self) -> CallbackReturn {
                let mut reqs = Vec::new();
                for i in 0..10 {
                    let url = format!("{}/{}", self.base_url.trim_end_matches('/'), i);
                    reqs.push(SpiderOutput::Request(
                        Request::get(&url).with_callback(counting_callback),
                    ));
                }
                Box::new(reqs.into_iter())
            }
        }

        // Build engine
        let spider = Box::new(BulkSpider {
            base_url: server_url.clone(),
        });
        let mut engine = Engine::new(spider, log::LevelFilter::Info);
        let _ = engine.run().await;

        let count = CALLBACK_COUNT.load(Ordering::SeqCst);
        assert_eq!(count, 10);
    }

    #[tokio::test]
    async fn test_engine_writes_item_to_file() {
        // Helper to create a unique temp file path
        fn unique_temp_path(name: &str) -> PathBuf {
            let ts = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let pid = std::process::id();
            std::env::temp_dir().join(format!("{name}_{pid}_{ts}.jsonl", name = name))
        }

        // Spider that emits one request and one item
        struct FileSpider {
            base_url: String,
            file_path: std::path::PathBuf,
        }

        impl FileSpider {
            fn callback(_: Response) -> CallbackReturn {
                let item: ItemBox = Box::new(json!({"hello": "world"}));
                Box::new(std::iter::once(SpiderOutput::Item(item)))
            }
        }

        impl Spider for FileSpider {
            fn settings(&self) -> SpiderSettings {
                crate::spider::SpiderSettings {
                    pipelines: vec![Box::new(JsonLinesExporter::new(self.file_path.clone()))],
                }
            }

            fn start(&self) -> CallbackReturn {
                let req = Request::get(&self.base_url).with_callback(Self::callback);
                Box::new(std::iter::once(SpiderOutput::Request(req)))
            }
        }

        // Start mock HTTP server
        let server = httpmock::MockServer::start();
        let _m = server.mock(|when, then| {
            when.method(httpmock::Method::GET).path("/");
            then.status(200).body("data");
        });
        let base_url = server.url("/");

        // Prepare spider with a known file path
        let path = unique_temp_path("engine_file_write");
        let spider = Box::new(FileSpider {
            base_url,
            file_path: path.clone(),
        });
        let mut engine = Engine::new(spider, log::LevelFilter::Info);

        // Run engine: download + callback + pipeline write
        let _ = engine.run().await;

        // Read and verify the JSONL output
        let contents = tokio::fs::read_to_string(&path).await.unwrap();
        // Should contain one line with {"hello":"world"}
        assert!(
            contents.contains(r#"{"hello":"world"}"#),
            "File did not contain expected JSON, got: {}",
            contents
        );
    }

    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;
    use tokio::task::JoinHandle;

    #[tokio::test]
    async fn engine_retry_middleware_invokes_expected_callbacks() {
        static REQUEST2_HITS: AtomicUsize = AtomicUsize::new(0);
        static CALLBACK1_COUNT: AtomicUsize = AtomicUsize::new(0);
        static CALLBACK2_COUNT: AtomicUsize = AtomicUsize::new(0);

        // Minimal HTTP server:
        // - /request1 → always 500
        // - /request2 → first 500, then 200 OK "ok"
        async fn start_mock_server() -> (String, JoinHandle<()>) {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();
            let handle = tokio::spawn(async move {
                loop {
                    match listener.accept().await {
                        Ok((mut socket, _)) => {
                            let mut buf = [0u8; 512];
                            let _ = socket.read(&mut buf).await;
                            let req = String::from_utf8_lossy(&buf);
                            if req.contains("GET /request1 ") {
                                let _ = socket.write_all(b"HTTP/1.1 500 Internal Server Error\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").await;
                            } else if req.contains("GET /request2 ") {
                                let n = REQUEST2_HITS.fetch_add(1, Ordering::SeqCst);
                                if n == 0 {
                                    let _ = socket.write_all(b"HTTP/1.1 500 Internal Server Error\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").await;
                                } else {
                                    let _ = socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok").await;
                                }
                            } else {
                                let _ = socket.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").await;
                            }
                            let _ = socket.shutdown().await;
                        }
                        Err(_) => break,
                    }
                }
            });
            (format!("http://{}", addr), handle)
        }

        struct TestSpider {
            base: String,
        }
        impl TestSpider {
            fn callback1(_resp: Response) -> CallbackReturn {
                CALLBACK1_COUNT.fetch_add(1, Ordering::SeqCst);
                Box::new(std::iter::empty())
            }
            fn callback2(_resp: Response) -> CallbackReturn {
                CALLBACK2_COUNT.fetch_add(1, Ordering::SeqCst);
                Box::new(std::iter::empty())
            }
        }
        impl Spider for TestSpider {
            fn start(&self) -> CallbackReturn {
                let url1 = &format!("{}/request1", self.base);
                let url2 = &format!("{}/request2", self.base);
                let requests = vec![
                    SpiderOutput::Request(Request::get(url1).with_callback(Self::callback1)),
                    SpiderOutput::Request(Request::get(url2).with_callback(Self::callback2)),
                ];
                Box::new(requests.into_iter())
            }
        }
        // reset global counters
        REQUEST2_HITS.store(0, Ordering::SeqCst);
        CALLBACK1_COUNT.store(0, Ordering::SeqCst);
        CALLBACK2_COUNT.store(0, Ordering::SeqCst);

        // start server
        let (base, server) = start_mock_server().await;

        // spin the engine with this spider
        let spider = TestSpider { base };
        let mut engine = Engine::new(Box::new(spider), log::LevelFilter::Info);
        let _ = engine.run().await;

        // Assertions:
        // /request1 always 500 → dropped by retry middleware → callback1 never called
        assert_eq!(
            CALLBACK1_COUNT.load(Ordering::SeqCst),
            0,
            "callback1 should never run"
        );
        // /request2 500 once then 200 → callback2 should run exactly once
        assert_eq!(
            CALLBACK2_COUNT.load(Ordering::SeqCst),
            1,
            "callback2 should run exactly once"
        );

        server.abort();
    }
}
