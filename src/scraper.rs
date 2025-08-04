use crate::{
    downloader::DownloadResult,
    item_pipeline::{PipelineError, PipelineManager},
    request::Request,
    response::Response,
    spider::{CallbackReturn, Spider, SpiderOutput},
};
use std::iter::Iterator;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use tokio::sync::{Mutex, mpsc};
use tokio::task::JoinHandle;

pub struct Scraper {
    spider: Box<dyn Spider>,
    spider_output_processor: SpiderOutputProcessor,
    item_pipeline_manager: Arc<Mutex<PipelineManager>>,
}

struct SpiderOutputProcessor {
    spider_output_tx: Option<mpsc::Sender<(CallbackReturn, bool)>>,
    handles: Vec<JoinHandle<()>>,
    pending_count: Arc<AtomicUsize>,
    active_count: Arc<AtomicUsize>,
}

struct SpiderOutputProcessorWorker {
    spider_output_rx: Arc<Mutex<mpsc::Receiver<(CallbackReturn, bool)>>>,
    spider_request_tx: Arc<Mutex<mpsc::Sender<Request>>>,
    pending_count: Arc<AtomicUsize>,
    active_count: Arc<AtomicUsize>,
    item_pipeline_manager: Arc<Mutex<PipelineManager>>,
}

impl Scraper {
    pub fn new(spider: Box<dyn Spider>, spider_request_tx: mpsc::Sender<Request>) -> Self {
        let settings = spider.settings();
        let item_pipeline_manager = Arc::new(Mutex::new(PipelineManager::new(settings.pipelines)));
        Self {
            spider,
            spider_output_processor: SpiderOutputProcessor::new(
                spider_request_tx,
                item_pipeline_manager.clone(),
            ),
            item_pipeline_manager: item_pipeline_manager,
        }
    }

    pub async fn start_spider(&self) {
        log::info!("starting spider...");
        let _ = self.item_pipeline_manager.lock().await.open().await;
        let spider_output = self.spider.start();
        self.spider_output_processor
            .enqueue_spider_output(spider_output, true)
            .await;
    }

    pub async fn process_response(&self, download_result: DownloadResult) {
        let response = Response::from(download_result);
        let spider_output = (response.request.callback)(response);
        // For start requests, pass `true`, for parsed callbacks, pass `false`
        self.spider_output_processor
            .enqueue_spider_output(spider_output, false)
            .await;
    }

    pub fn is_idle(&self) -> bool {
        self.spider_output_processor.is_idle()
    }
    pub async fn stop(&mut self) {
        self.spider_output_processor.stop().await;
        let _ = self.item_pipeline_manager.lock().await.close().await;
    }
}

impl SpiderOutputProcessor {
    const SPIDER_OUTPUT_WORKERS: usize = 10;

    pub fn new(
        spider_request_tx: mpsc::Sender<Request>,
        item_pipeline_manager: Arc<Mutex<PipelineManager>>,
    ) -> Self {
        let (spider_output_tx, spider_output_rx) =
            mpsc::channel::<(CallbackReturn, bool)>(Self::SPIDER_OUTPUT_WORKERS * 10);
        let pending_count = Arc::new(AtomicUsize::new(0));
        let active_count = Arc::new(AtomicUsize::new(0));
        let mut spider_output_processor = SpiderOutputProcessor {
            spider_output_tx: Some(spider_output_tx),
            handles: Vec::new(),
            pending_count: pending_count.clone(),
            active_count: active_count.clone(),
        };
        // Start the worker pool
        spider_output_processor.start(spider_output_rx, spider_request_tx, item_pipeline_manager);
        spider_output_processor
    }

    fn start(
        &mut self,
        spider_output_rx: mpsc::Receiver<(CallbackReturn, bool)>,
        spider_request_tx: mpsc::Sender<Request>,
        item_pipeline_manager: Arc<Mutex<PipelineManager>>,
    ) {
        let shared_rx = Arc::new(Mutex::new(spider_output_rx));
        let shared_tx = Arc::new(Mutex::new(spider_request_tx));
        for _ in 0..Self::SPIDER_OUTPUT_WORKERS {
            let mut worker = SpiderOutputProcessorWorker::new(
                shared_rx.clone(),
                shared_tx.clone(),
                Arc::clone(&self.pending_count),
                Arc::clone(&self.active_count),
                item_pipeline_manager.clone(),
            );
            let handle = tokio::spawn(async move {
                worker.process_spider_output().await;
            });
            self.handles.push(handle);
        }
    }

    pub async fn stop(&mut self) {
        if let Some(spider_output_tx) = self.spider_output_tx.take() {
            drop(spider_output_tx);
        }
        for handle in self.handles.drain(..) {
            let _ = handle.await;
        }
    }

    pub async fn enqueue_spider_output(&self, spider_output: CallbackReturn, is_start: bool) {
        self.pending_count.fetch_add(1, Ordering::SeqCst);
        if let Some(spider_output_tx) = &self.spider_output_tx {
            if let Err(_) = spider_output_tx.send((spider_output, is_start)).await {
                panic!("spider output queue receiver dropped! This should not happen!");
            }
        }
    }

    pub fn is_idle(&self) -> bool {
        self.pending_count.load(Ordering::SeqCst) == 0
            && self.active_count.load(Ordering::SeqCst) == 0
    }
}

impl SpiderOutputProcessorWorker {
    fn new(
        spider_output_rx: Arc<Mutex<mpsc::Receiver<(CallbackReturn, bool)>>>,
        spider_request_tx: Arc<Mutex<mpsc::Sender<Request>>>,
        pending_count: Arc<AtomicUsize>,
        active_count: Arc<AtomicUsize>,
        item_pipeline_manager: Arc<Mutex<PipelineManager>>,
    ) -> Self {
        Self {
            spider_output_rx,
            spider_request_tx,
            pending_count,
            active_count,
            item_pipeline_manager,
        }
    }

    async fn process_spider_output(&mut self) {
        loop {
            let maybe = {
                let mut guard = self.spider_output_rx.lock().await;
                guard.recv().await
            };
            match maybe {
                Some((mut spider_output, is_start)) => {
                    self.pending_count.fetch_sub(1, Ordering::SeqCst);
                    self.active_count.fetch_add(1, Ordering::SeqCst);
                    while let Some(output) = spider_output.next() {
                        match output {
                            SpiderOutput::Item(item) => {
                                let item_pipeline = self.item_pipeline_manager.lock().await;
                                match item_pipeline.process(item).await {
                                    Ok(item) => {
                                        log::debug!("processed item: {:?}", &item);
                                    }
                                    Err(PipelineError::Drop(why)) => {
                                        log::warn!("dropped item, reason: {}", why);
                                    }
                                    Err(PipelineError::Other(err)) => {
                                        log::error!("error in processing iten, reason: {:?}", err);
                                    }
                                }
                            }
                            SpiderOutput::Request(request) => {
                                let tx_guard = self.spider_request_tx.lock().await;
                                // Determine whether to mark this as a start request
                                let req = if is_start {
                                    request.as_start()
                                } else {
                                    request
                                };
                                // TODO(Voith): Implement Middleware for request
                                let _ = tx_guard.send(req).await;
                            }
                        }
                    }
                    self.active_count.fetch_sub(1, Ordering::SeqCst);
                }
                None => break, // channel closed
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;
    use http::Response as HttpResponse;
    use reqwest::Response as ReqwestResponse;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use tokio::sync::mpsc;

    // Dummy spider that yields no start requests
    struct DummySpider;
    impl Spider for DummySpider {
        fn start(&self) -> CallbackReturn {
            Box::new(std::iter::empty())
        }
    }

    #[tokio::test]
    async fn test_start_spider() {
        static FLAG: AtomicBool = AtomicBool::new(false);
        // Spider that yields one start request
        struct OneSpider;
        impl Spider for OneSpider {
            fn start(&self) -> CallbackReturn {
                let req = Request::get("http://example.com").with_callback(|_: Response| {
                    FLAG.store(true, Ordering::SeqCst);
                    Box::new(std::iter::empty())
                });
                Box::new(std::iter::once(SpiderOutput::Request(req)))
            }
        }

        let (tx, mut rx) = mpsc::channel(1);
        let scraper = Scraper::new(Box::new(OneSpider), tx);
        scraper.start_spider().await;

        // Should receive exactly one start request
        let request = rx.recv().await.expect("Expected a start request");
        assert!(request.is_start_request(), "Request should be marked start");
    }

    #[tokio::test]
    async fn test_process_response_callback_called() {
        static CALLED: AtomicBool = AtomicBool::new(false);

        // Setup a DownloadResult with a callback that sets CALLED
        let req = Request::get("http://example.com").with_callback(|_: Response| {
            CALLED.store(true, Ordering::SeqCst);
            Box::new(std::iter::empty())
        });
        let http_res = HttpResponse::builder()
            .status(200)
            .body(Vec::new())
            .unwrap();
        let reqwest_res = ReqwestResponse::from(http_res);
        let download_result = DownloadResult {
            request: req,
            status: reqwest_res.status(),
            headers: reqwest_res.headers().clone(),
            body: Bytes::new(),
            text: "".to_string(),
        };

        let (tx, _rx) = mpsc::channel(1);
        let scraper = Scraper::new(Box::new(DummySpider), tx);
        scraper.process_response(download_result).await;

        assert!(CALLED.load(Ordering::SeqCst), "Callback was not invoked");
    }

    #[tokio::test]
    async fn test_process_response_enqueues_request() {
        static COUNT: AtomicUsize = AtomicUsize::new(0);

        // Callback that yields one new Request
        fn yield_request(_: Response) -> CallbackReturn {
            COUNT.fetch_add(1, Ordering::SeqCst);
            let new_req = Request::get("http://parsed")
                .with_callback(|_: Response| Box::new(std::iter::empty()));
            Box::new(std::iter::once(SpiderOutput::Request(new_req)))
        }

        let req = Request::get("http://example.com").with_callback(yield_request);
        let http_res = HttpResponse::builder()
            .status(200)
            .body(Vec::new())
            .unwrap();
        let reqwest_res = ReqwestResponse::from(http_res);
        let download_result = DownloadResult {
            request: req,
            status: reqwest_res.status(),
            headers: reqwest_res.headers().clone(),
            body: Bytes::new(),
            text: "".to_string(),
        };

        let (tx, mut rx) = mpsc::channel(1);
        let scraper = Scraper::new(Box::new(DummySpider), tx);
        scraper.process_response(download_result).await;

        // Should receive the new request (not marked start)
        let request = rx.recv().await.expect("Expected a request");
        assert!(
            !request.is_start_request(),
            "Parsed request should not be start"
        );
        assert_eq!(
            COUNT.load(Ordering::SeqCst),
            1,
            "Callback should have been called once"
        );
    }

    #[tokio::test]
    async fn test_scraper_is_idle_after_processing() {
        // Spider that yields one start request
        struct OneSpider;
        impl Spider for OneSpider {
            fn start(&self) -> CallbackReturn {
                Box::new(std::iter::once(SpiderOutput::Request(
                    Request::get("http://example.com")
                        .with_callback(|_: Response| Box::new(std::iter::empty())),
                )))
            }
        }

        let (tx, mut rx) = mpsc::channel(1);
        let scraper = Scraper::new(Box::new(OneSpider), tx);

        // Initially idle
        assert!(scraper.is_idle(), "Scraper should start idle");

        // Start spider (enqueue and process)
        scraper.start_spider().await;
        // Received start request
        let _ = rx.recv().await.unwrap();
        // Not idle while processing
        assert!(
            scraper.is_idle(),
            "Scraper should be idle after processing requests"
        );
    }

    #[tokio::test]
    async fn test_stop() {
        let (tx, _) = mpsc::channel::<Request>(1);
        let mut scraper = Scraper::new(Box::new(DummySpider), tx);
        // Calling stop on idle scraper should not panic and stay idle
        scraper.stop().await;
        assert!(scraper.is_idle(), "Scraper should remain idle after stop");
    }
}
