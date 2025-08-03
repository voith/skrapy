use crate::downloader::{DownloadError, DownloadManager, DownloadResult};
use crate::request::Request;
use crate::scheduler::Scheduler;
use crate::scraper::Scraper;
use crate::spider::Spider;
use std::sync::Arc;
use tokio::signal;
use tokio::sync::{Mutex, mpsc};
use tokio::time::Duration;
use tokio_util::sync::CancellationToken;

pub struct Engine {
    download_manager: DownloadManager,
    scraper: Scraper,
    scheduler: Scheduler,
    downloader_output_rx: mpsc::Receiver<Result<DownloadResult, DownloadError>>,
    spider_request_rx: mpsc::Receiver<Request>,
    shutdown: CancellationToken,
}

impl Engine {
    const SCRAPER_QUEUE_SIZE: usize = 1000;
    const CONCURRENCY_LIMIT: usize = 8;
    const DOWNLOADER_OUTPUT_QUEUE: usize = Self::CONCURRENCY_LIMIT * 100;

    pub fn new(spider: Box<dyn Spider>) -> Self {
        let (downloader_output_tx, downloader_output_rx) =
            mpsc::channel(Self::DOWNLOADER_OUTPUT_QUEUE);
        let (spider_request_tx, spider_request_rx) = mpsc::channel(Self::SCRAPER_QUEUE_SIZE);
        let scraper = Scraper::new(spider, spider_request_tx);
        let scheduler = Scheduler::new();
        let shutdown = CancellationToken::new();
        Self {
            download_manager: DownloadManager::new(
                Self::CONCURRENCY_LIMIT,
                Arc::new(Mutex::new(downloader_output_tx)),
            ),
            scraper,
            scheduler,
            downloader_output_rx,
            spider_request_rx,
            shutdown,
        }
    }

    fn shutdown_if_idle(&self) {
        if self.download_manager.is_idle()
            && self.scraper.is_idle()
            && !self.scheduler.has_pending_requests()
            && self.downloader_output_rx.is_empty()
            && self.spider_request_rx.is_empty()
        {
            self.shutdown.cancel();
        }
    }

    pub async fn run(&mut self) {
        self.scraper.start_spider().await;
        let mut idle_interval = tokio::time::interval(Duration::from_millis(100));
        loop {
            tokio::select! {
                // Shutdown on Ctrl+C (SIGINT)
                _ = signal::ctrl_c() => {
                    println!("Received Ctrl+C, shutting down engine");
                    self.shutdown.cancel();
                },
                // Stop when Shutdown
                _ = self.shutdown.cancelled() => {
                    println!("stopping engine");
                    self.download_manager.stop().await;
                    self.scraper.stop().await;
                    break;
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
                Some(download_output) = self.downloader_output_rx.recv() => {
                    match download_output {
                        Ok(download_result) => {
                            self.scraper.process_response(download_result).await;
                        }
                        Err(_download_error) => {
                            //TODO(Voith): Handle Download Error
                            println!("Encountered download error");
                        }
                    }
                },
                _ = idle_interval.tick() => {},
            }
            // shudown if idle
            self.shutdown_if_idle();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::items::Item;
    use crate::response::Response;
    use crate::spider::{CallbackReturn, Spider, SpiderOutput};
    use httpmock::Method::GET;
    use httpmock::MockServer;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

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
        let mut engine = Engine::new(spider);

        // Run the engine; it should process the one request and then shut itself down
        engine.run().await;

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
            Box::new(std::iter::once(SpiderOutput::Item(Item::default())))
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
        let mut engine = Engine::new(spider);
        engine.run().await;

        let count = CALLBACK_COUNT.load(Ordering::SeqCst);
        assert_eq!(count, 10);
    }
}
