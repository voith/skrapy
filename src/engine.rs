use crate::downloader::{DownloadError, DownloadManager, DownloadResult};
use crate::request::{CallbackReturn, Request};
use crate::scraper::Scraper;
use crate::spider::Spider;
use std::sync::Arc;
use tokio::signal;
use tokio::sync::{Mutex, mpsc};
use tokio_util::sync::CancellationToken;

pub struct Engine {
    download_manager: DownloadManager,
    scraper: Scraper,
    request_queue_tx: mpsc::Sender<Request>,
    request_queue_rx: mpsc::Receiver<Request>,
    downloader_output_rx: mpsc::Receiver<Result<DownloadResult, DownloadError>>,
    download_result_queue_tx: mpsc::Sender<DownloadResult>,
    download_result_queue_rx: mpsc::Receiver<DownloadResult>,
    spider_output_tx: mpsc::Sender<CallbackReturn>,
    spider_output_rx: mpsc::Receiver<CallbackReturn>,
    shutdown: CancellationToken,
    are_start_requested_enqueued: bool,
}

impl Engine {
    const SCRAPER_QUEUE_SIZE: usize = 1000;
    const CONCURRENCY_LIMIT: usize = 8;
    const DOWNLOADER_OUTPUT_QUEUE: usize = Self::CONCURRENCY_LIMIT * 100;
    const REQUEST_QUEUE_SIZE: usize = 10000;

    pub fn new(spider: Box<dyn Spider>) -> Self {
        let (request_queue_tx, request_queue_rx) = mpsc::channel(Self::REQUEST_QUEUE_SIZE);
        let (downloader_output_tx, downloader_output_rx) =
            mpsc::channel(Self::DOWNLOADER_OUTPUT_QUEUE);

        let (download_result_queue_tx, download_result_queue_rx) =
            mpsc::channel(Self::SCRAPER_QUEUE_SIZE);
        let (spider_output_tx, spider_output_rx) = mpsc::channel(Self::SCRAPER_QUEUE_SIZE);
        let scraper = Scraper::new(spider);
        let shutdown = CancellationToken::new();
        Self {
            download_manager: DownloadManager::new(
                Self::CONCURRENCY_LIMIT,
                Arc::new(Mutex::new(downloader_output_tx)),
            ),
            scraper,
            request_queue_tx,
            request_queue_rx,
            downloader_output_rx,
            download_result_queue_tx,
            download_result_queue_rx,
            spider_output_tx,
            spider_output_rx,
            shutdown,
            are_start_requested_enqueued: false,
        }
    }

    fn shudown_if_idle(&self) {
        if self.are_start_requested_enqueued
            && self.download_manager.is_idle()
            && self.request_queue_rx.is_empty()
            && self.download_result_queue_rx.is_empty()
            && self.downloader_output_rx.is_empty()
        {
            self.shutdown.cancel();
        }
    }

    pub async fn run(&mut self) {
        let mut start_requests = self.scraper.generate_start_requests();
        loop {
            tokio::select! {
                // Lazy enqueue start requests only when there's capacity
                Some(request) = async {
                    if !self.are_start_requested_enqueued && self.request_queue_tx.capacity() > 0 {
                        match start_requests.next() {
                            Some(req) => Some(req),
                            None => {
                                self.are_start_requested_enqueued = true;
                                None
                            }
                        }

                    } else {
                        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                        None
                    }
                } => {
                    self.enqueue_request(request).await;
                },
                // fetch downloaded requests and pass them to the spider
                Some(download_result) = self.download_result_queue_rx.recv() => {
                    let spider_output = self.scraper.process_response(download_result).await;
                    self.enqueue_spider_output(spider_output).await;
                },
                // fetch spider output and send it for processing
                Some(spider_output) = self.spider_output_rx.recv() => {
                    self.scraper.process_spider_output(spider_output).await;
                },
                // fetch new requests
                Some(request) = self.request_queue_rx.recv() => {
                    self.download_manager.enqueue_request(request).await;
                }
                Some(download_output) = self.downloader_output_rx.recv() => {
                    match download_output {
                        Ok(download_result) => {
                            self.enqueue_download_result(download_result).await;
                        }
                        Err(download_error) => {
                            //TODO(Voith): Handle Download Error
                            println!("Encountered download error");
                        }
                    }
                },
                // Shutdown on Ctrl+C (SIGINT)
                _ = signal::ctrl_c() => {
                    println!("Received Ctrl+C, shutting down engine");
                    self.shutdown.cancel();
                },
                // Stop when Shutdown
                _ = self.shutdown.cancelled() => {
                    println!("stopping engine");
                    self.download_manager.stop().await;
                    break;
                },
            }
            // shudown if idle
            self.shudown_if_idle();
        }
    }

    async fn enqueue_request(&self, request: Request) {
        if let Err(_) = self.request_queue_tx.send(request).await {
            panic!("request queue receiver dropped! This should not happen!");
        }
    }

    async fn enqueue_download_result(&self, download_result: DownloadResult) {
        if let Err(_) = self.download_result_queue_tx.send(download_result).await {
            panic!("download queue receiver dropped! This should not happen!");
        }
    }

    async fn enqueue_spider_output(&self, spider_output: CallbackReturn) {
        if let Err(_) = self.spider_output_tx.send(spider_output).await {
            panic!("spider output queue receiver dropped! This should not happen!");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::response::Response;
    use httpmock::Method::GET;
    use httpmock::MockServer;
    use std::sync::atomic::{AtomicBool, Ordering};

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
        fn start(&self) -> Box<dyn Iterator<Item = Request> + Send> {
            let request = Request::get(&self.base_url)
                .with_callback(dummy_callback)
                .as_start();
            Box::new(vec![request].into_iter())
        }
    }

    #[tokio::test]
    async fn test_engine_handles_one_request_and_shuts_down() {
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
}
