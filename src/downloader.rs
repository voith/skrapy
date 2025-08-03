use crate::request::Request;
use reqwest::Client;
use reqwest::Response as ReqwestResponse;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use tokio::sync::{Mutex, mpsc};
use tokio::task::JoinHandle;

pub struct DownloadResult {
    pub request: Request,
    pub response: ReqwestResponse,
}

/// Wraps low-level HTTP errors.
pub struct HttpError {
    pub source: reqwest::Error,
}

/// Error returned when a download fails, containing the original request.
pub struct DownloadError {
    pub request: Request,
    pub error: HttpError,
}

pub struct DownloadManager {
    concurrency_limit: usize,
    client: Client,
    handles: Vec<JoinHandle<()>>,
    sender: Option<mpsc::Sender<Request>>,
    active_count: Arc<AtomicUsize>,
    pending_count: Arc<AtomicUsize>,
}

pub struct Downloader {
    client: Client,
    receiver: Arc<Mutex<mpsc::Receiver<Request>>>,
    result_tx: Arc<Mutex<mpsc::Sender<Result<DownloadResult, DownloadError>>>>,
    active_count: Arc<AtomicUsize>,
    pending_count: Arc<AtomicUsize>,
}

impl DownloadManager {
    pub fn new(
        concurrency_limit: usize,
        result_tx: Arc<Mutex<mpsc::Sender<Result<DownloadResult, DownloadError>>>>,
    ) -> Self {
        let active_count = Arc::new(AtomicUsize::new(0));
        let pending_count = Arc::new(AtomicUsize::new(0));
        let (sender, receiver) = mpsc::channel(concurrency_limit * 2);
        let mut manager = DownloadManager {
            concurrency_limit,
            client: Client::new(),
            handles: Vec::new(),
            sender: Some(sender),
            active_count: active_count,
            pending_count: pending_count,
        };
        manager.start(receiver, result_tx);
        manager
    }

    fn start(
        &mut self,
        receiver: mpsc::Receiver<Request>,
        result_tx: Arc<Mutex<mpsc::Sender<Result<DownloadResult, DownloadError>>>>,
    ) {
        let shared_rx = Arc::new(Mutex::new(receiver));
        for _ in 0..self.concurrency_limit as usize {
            let client = self.client.clone();
            let rx = shared_rx.clone();
            let result_tx_clone = result_tx.clone();
            let active_count = Arc::clone(&self.active_count);
            let pending_count = Arc::clone(&self.pending_count);
            let mut downloader =
                Downloader::new(client, rx, result_tx_clone, active_count, pending_count);
            let handle = tokio::spawn(async move {
                downloader.handle_incoming_requests().await;
            });
            self.handles.push(handle);
        }
    }

    pub async fn stop(&mut self) {
        if let Some(sender) = self.sender.take() {
            drop(sender);
        }
        for handle in self.handles.drain(..) {
            let _ = handle.await;
        }
    }

    pub async fn enqueue_request(&self, request: Request) {
        self.pending_count.fetch_add(1, Ordering::SeqCst);
        if let Some(sender) = &self.sender {
            let _ = sender.send(request).await;
        }
    }

    pub fn is_idle(&self) -> bool {
        self.pending_count.load(Ordering::SeqCst) == 0
            && self.active_count.load(Ordering::SeqCst) == 0
    }

    pub fn needs_backoff(&self) -> bool {
        // back off if the queue is full
        self.pending_count.load(Ordering::SeqCst) == (self.concurrency_limit * 2)
    }
}

// TODO List
// - Timeout Handling
// - Download Delay
// - logging
impl Downloader {
    pub fn new(
        client: Client,
        receiver: Arc<Mutex<mpsc::Receiver<Request>>>,
        result_tx: Arc<Mutex<mpsc::Sender<Result<DownloadResult, DownloadError>>>>,
        active_count: Arc<AtomicUsize>,
        pending_count: Arc<AtomicUsize>,
    ) -> Self {
        Downloader {
            client,
            receiver,
            result_tx,
            active_count,
            pending_count,
        }
    }

    /// Perform the HTTP request using reqwest and return the raw response.
    async fn download(&self, request: &Request) -> Result<ReqwestResponse, reqwest::Error> {
        // Build request
        let mut builder = self
            .client
            .request(request.method.clone(), request.url.clone());

        // Apply headers
        for (name, value) in request.headers.iter() {
            builder = builder.header(name, value);
        }

        // Apply body if present
        if let Some(bytes) = request.body.as_bytes() {
            builder = builder.body(bytes.to_vec());
        }

        // Send and return the response
        builder.send().await
    }

    /// Listen for download requests and process them.
    pub async fn handle_incoming_requests(&mut self) {
        loop {
            // Lock the receiver to pop a request
            let mut rx = self.receiver.lock().await;
            match rx.recv().await {
                Some(req) => {
                    self.pending_count.fetch_sub(1, Ordering::SeqCst);
                    // Release lock before download
                    drop(rx);
                    self.active_count.fetch_add(1, Ordering::SeqCst);
                    match self.download(&req).await {
                        Ok(response) => {
                            let result = DownloadResult {
                                request: req,
                                response,
                            };
                            let _ = self.result_tx.lock().await.send(Ok(result)).await;
                        }
                        Err(err) => {
                            let dl_err = DownloadError {
                                request: req,
                                error: HttpError { source: err },
                            };
                            let _ = self.result_tx.lock().await.send(Err(dl_err)).await;
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
    use super::{DownloadManager, Downloader};
    use crate::request::{Body, Request};
    use httpmock::{Method::GET, MockServer};
    use reqwest::Client;
    use reqwest::header::HeaderMap;
    use reqwest::{Method, Url};
    use std::sync::Arc;
    use std::sync::atomic::AtomicUsize;
    use tokio::sync::Mutex;
    use tokio::sync::mpsc;
    use tokio::time::{Duration, timeout};

    fn dummy_callback(
        _: crate::response::Response,
    ) -> Box<dyn Iterator<Item = crate::spider::SpiderOutput> + Send> {
        Box::new(std::iter::empty())
    }

    #[tokio::test]
    async fn test_download_manager_multiple_downloads() {
        // Start a mock HTTP server
        let server = MockServer::start();
        let _mock = server.mock(|when, then| {
            when.method(GET).path("/test");
            then.status(200).body("ok");
        });

        // Channel for receiving download results
        let (tx_res, mut rx_res) = mpsc::channel(10);
        let result_tx = Arc::new(Mutex::new(tx_res));

        // Initialize DownloadManager with concurrency limit and result transmitter
        let manager = DownloadManager::new(4, result_tx);

        // Enqueue multiple requests
        for _ in 0..5 {
            let request = Request::new(
                Url::parse(&format!("{}/test", server.base_url())).unwrap(),
                Method::GET,
                HeaderMap::new(),
                Body::default(),
                dummy_callback,
                0,
                0,
                false,
            );
            manager.enqueue_request(request).await;
        }

        // Collect and verify responses
        for _ in 0..5 {
            let download_result = timeout(Duration::from_secs(2), rx_res.recv())
                .await
                .expect("timed out")
                .expect("response channel closed");
            match download_result {
                Ok(res) => {
                    assert_eq!(res.response.status(), 200);
                    assert_eq!(
                        res.request.url.as_str(),
                        &format!("{}/test", server.base_url())
                    );
                }
                Err(_) => panic!("Expected Ok DownloadResult"),
            }
        }
    }

    #[tokio::test]
    async fn test_download_function_success() {
        let server = MockServer::start();
        let _mock = server.mock(|when, then| {
            when.method(GET).path("/ok");
            then.status(200).body("hello");
        });

        let client = Client::new();
        let request = Request::new(
            Url::parse(&format!("{}/ok", server.base_url())).unwrap(),
            Method::GET,
            HeaderMap::new(),
            Body::default(),
            dummy_callback,
            0,
            0,
            false,
        );
        let downloader = Downloader::new(
            client,
            Arc::new(Mutex::new(mpsc::channel(1).1)),
            Arc::new(Mutex::new(mpsc::channel(1).0)),
            Arc::new(AtomicUsize::new(0)),
            Arc::new(AtomicUsize::new(0)),
        );
        let response = downloader.download(&request).await.unwrap();
        let text = response.text().await.unwrap();
        assert_eq!(text, "hello");
    }

    #[tokio::test]
    async fn test_handle_incoming_requests_error() {
        // Use a URL that will fail (e.g., invalid port)
        let request = Request::new(
            Url::parse("http://127.0.0.1:0/fail").unwrap(),
            Method::GET,
            HeaderMap::new(),
            Body::default(),
            dummy_callback,
            0,
            0,
            false,
        );
        let (tx_req, rx_req) = mpsc::channel(1);
        let (tx_res, mut rx_res) = mpsc::channel(1);
        let result_tx = Arc::new(Mutex::new(tx_res));
        let mut downloader = Downloader::new(
            Client::new(),
            Arc::new(Mutex::new(rx_req)),
            result_tx.clone(),
            Arc::new(AtomicUsize::new(0)),
            Arc::new(AtomicUsize::new(0)),
        );

        // Spawn the handler
        tokio::spawn(async move {
            downloader.handle_incoming_requests().await;
        });

        // Send the request and close
        tx_req.send(request.clone()).await.unwrap();
        drop(tx_req);

        // Receive result
        if let Ok(Some(Err(dl_err))) = timeout(Duration::from_secs(1), rx_res.recv()).await {
            assert_eq!(dl_err.request.url, request.url);
        } else {
            panic!("Expected a DownloadError");
        }
    }

    #[tokio::test]
    async fn test_handle_incoming_requests_success() {
        let server = MockServer::start();
        let _mock = server.mock(|when, then| {
            when.method(GET).path("/ok");
            then.status(200).body("data");
        });

        let request = Request::new(
            Url::parse(&format!("{}/ok", server.base_url())).unwrap(),
            Method::GET,
            HeaderMap::new(),
            Body::default(),
            dummy_callback,
            0,
            0,
            false,
        );
        let (tx_req, rx_req) = mpsc::channel(1);
        let (tx_res, mut rx_res) = mpsc::channel(1);
        let result_tx = Arc::new(Mutex::new(tx_res));
        let mut downloader = Downloader::new(
            Client::new(),
            Arc::new(Mutex::new(rx_req)),
            result_tx.clone(),
            Arc::new(AtomicUsize::new(0)),
            Arc::new(AtomicUsize::new(0)),
        );

        tokio::spawn(async move {
            downloader.handle_incoming_requests().await;
        });

        tx_req.send(request.clone()).await.unwrap();
        drop(tx_req);

        if let Ok(Some(Ok(dl_res))) = timeout(Duration::from_secs(1), rx_res.recv()).await {
            assert_eq!(dl_res.request.url, request.url);
            let body = dl_res.response.text().await.unwrap();
            assert_eq!(body, "data");
        } else {
            panic!("Expected a DownloadResult");
        }
    }

    #[tokio::test]
    async fn test_download_manager_is_idle() {
        let (tx_res, _rx_res) = mpsc::channel(10);
        let result_tx = Arc::new(Mutex::new(tx_res));

        let manager = DownloadManager::new(2, result_tx.clone());

        // Should be idle initially
        assert!(manager.is_idle());

        // Enqueue a request
        let request = Request::new(
            Url::parse("http://example.com").unwrap(),
            Method::GET,
            HeaderMap::new(),
            Body::default(),
            dummy_callback,
            0,
            0,
            false,
        );
        manager.enqueue_request(request).await;

        // Slight delay to let worker pick it up
        tokio::time::sleep(Duration::from_millis(100)).await;

        // Should not be idle as a task is in progress
        let is_idle = manager.is_idle();
        assert!(!is_idle);
    }

    #[tokio::test]
    async fn test_download_manager_idle_after_all_downloads() {
        // Start mock server
        let server = MockServer::start();
        let _mock = server.mock(|when, then| {
            when.method(GET).path("/check");
            then.status(200).body("ok");
        });

        let (tx_res, mut rx_res) = mpsc::channel(10);
        let result_tx = Arc::new(Mutex::new(tx_res));
        let manager = DownloadManager::new(2, result_tx.clone());

        // Enqueue multiple requests
        let num_requests = 5;
        for _ in 0..num_requests {
            let request = Request::new(
                Url::parse(&format!("{}/check", server.base_url())).unwrap(),
                Method::GET,
                HeaderMap::new(),
                Body::default(),
                dummy_callback,
                0,
                0,
                false,
            );
            manager.enqueue_request(request).await;
        }

        // Collect all responses
        for _ in 0..num_requests {
            let download_result = timeout(Duration::from_secs(2), rx_res.recv())
                .await
                .expect("timeout")
                .expect("channel closed");
            assert!(download_result.is_ok());
        }

        // Slight delay to ensure all tasks completed
        tokio::time::sleep(Duration::from_millis(200)).await;

        // Ensure the manager is idle again
        assert!(manager.is_idle());
    }

    #[tokio::test]
    async fn test_download_manager_stop() {
        let server = MockServer::start();
        let _mock = server.mock(|when, then| {
            when.method(GET).path("/stop");
            then.status(200).body("done");
        });

        let (tx_res, _rx_res) = mpsc::channel(10);
        let result_tx = Arc::new(Mutex::new(tx_res));
        let mut manager = DownloadManager::new(2, result_tx.clone());

        let request = Request::new(
            Url::parse(&format!("{}/stop", server.base_url())).unwrap(),
            Method::GET,
            HeaderMap::new(),
            Body::default(),
            dummy_callback,
            0,
            0,
            false,
        );
        manager.enqueue_request(request).await;

        manager.stop().await;

        // The sender should be dropped
        assert!(manager.sender.is_none());

        // After stopping, the manager should report as idle
        assert!(manager.is_idle());
    }
}
