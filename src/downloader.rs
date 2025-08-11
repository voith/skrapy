use crate::{
    download_middleware::{DownloadMiddlewareProcessor, RetryOnErrorMiddleware},
    request::Request,
    response::Response,
};
use reqwest::Client;
use reqwest::Response as ReqwestResponse;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use tokio::sync::{Mutex, mpsc};
use tokio::task::JoinHandle;

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
    download_middleware_processor: Arc<Mutex<DownloadMiddlewareProcessor>>,
    receiver: Arc<Mutex<mpsc::Receiver<Request>>>,
    response_tx: mpsc::Sender<Response>,
    retry_request_tx: mpsc::Sender<Request>,
    active_count: Arc<AtomicUsize>,
    pending_count: Arc<AtomicUsize>,
}

impl DownloadManager {
    pub fn new(
        concurrency_limit: usize,
        response_tx: mpsc::Sender<Response>,
        retry_request_tx: mpsc::Sender<Request>,
    ) -> Self {
        let active_count = Arc::new(AtomicUsize::new(0));
        let pending_count = Arc::new(AtomicUsize::new(0));
        let (sender, receiver) = mpsc::channel(concurrency_limit * 2);
        // TODO: make max_retries configurable via settings/env; default 3 for now
        let max_retries = 3 as u32;
        let mut mwp = DownloadMiddlewareProcessor::new();
        mwp.add(RetryOnErrorMiddleware::new(max_retries));
        let mw_processor = Arc::new(Mutex::new(mwp));
        let mut manager = DownloadManager {
            concurrency_limit,
            client: Client::new(),
            handles: Vec::new(),
            sender: Some(sender),
            active_count: active_count,
            pending_count: pending_count,
        };
        manager.start(receiver, response_tx, mw_processor, retry_request_tx);
        manager
    }

    fn start(
        &mut self,
        receiver: mpsc::Receiver<Request>,
        response_tx: mpsc::Sender<Response>,
        download_middleware_processor: Arc<Mutex<DownloadMiddlewareProcessor>>,
        retry_request_tx: mpsc::Sender<Request>,
    ) {
        let shared_rx = Arc::new(Mutex::new(receiver));
        for _ in 0..self.concurrency_limit as usize {
            let client = self.client.clone();
            let rx = shared_rx.clone();
            let response_tx_clone = response_tx.clone();
            let active_count = Arc::clone(&self.active_count);
            let pending_count = Arc::clone(&self.pending_count);
            let mwp = download_middleware_processor.clone();
            let retry_tx = retry_request_tx.clone();
            let mut downloader = Downloader::new(
                client,
                mwp,
                rx,
                response_tx_clone,
                retry_tx,
                active_count,
                pending_count,
            );
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

// TODO(Voith): Add download delay between requests
impl Downloader {
    pub fn new(
        client: Client,
        download_middleware_processor: Arc<Mutex<DownloadMiddlewareProcessor>>,
        receiver: Arc<Mutex<mpsc::Receiver<Request>>>,
        response_tx: mpsc::Sender<Response>,
        retry_request_tx: mpsc::Sender<Request>,
        active_count: Arc<AtomicUsize>,
        pending_count: Arc<AtomicUsize>,
    ) -> Self {
        Downloader {
            client,
            download_middleware_processor,
            receiver,
            response_tx,
            retry_request_tx,
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
                        Ok(resp) => {
                            // Treat non-success HTTP status codes as errors (4xx/5xx)
                            match resp.error_for_status() {
                                Ok(ok_resp) => {
                                    let status = ok_resp.status();
                                    let headers = ok_resp.headers().clone();
                                    match ok_resp.bytes().await {
                                        Ok(body) => {
                                            log::debug!("downloaded request: {:?}", &req);
                                            let text = String::from_utf8_lossy(&body).to_string();
                                            let response = Response {
                                                request: req,
                                                status,
                                                headers,
                                                body: body.clone(),
                                                text,
                                            };
                                            let _ = self.response_tx.send(response).await;
                                        }
                                        // Response body Error
                                        Err(err) => {
                                            let dl_err = DownloadError {
                                                request: req,
                                                error: HttpError { source: err },
                                            };
                                            self.handle_download_error(dl_err).await;
                                        }
                                    }
                                }
                                // Http status code error
                                Err(err) => {
                                    let dl_err = DownloadError {
                                        request: req,
                                        error: HttpError { source: err },
                                    };
                                    self.handle_download_error(dl_err).await;
                                }
                            }
                        }
                        Err(err) => {
                            let dl_err = DownloadError {
                                request: req,
                                error: HttpError { source: err },
                            };
                            self.handle_download_error(dl_err).await;
                        }
                    }
                    self.active_count.fetch_sub(1, Ordering::SeqCst);
                }
                None => break, // channel closed
            }
        }
    }

    async fn handle_download_error(&self, dl_err: DownloadError) {
        let guard = self.download_middleware_processor.lock().await;
        if let Some(retry_req) = guard.process_error(dl_err) {
            let _ = self.retry_request_tx.send(retry_req).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::request::{Body, Request};
    use crate::spider::CallbackReturn;
    use httpmock::{Method::GET, MockServer};
    use reqwest::Client;
    use reqwest::header::HeaderMap;
    use reqwest::{Method, Url};
    use std::sync::Arc;
    use std::sync::atomic::AtomicUsize;
    use tokio::sync::Mutex;
    use tokio::sync::mpsc;
    use tokio::time::{Duration, timeout};

    fn dummy_callback(_: crate::response::Response) -> CallbackReturn {
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

        // Channel for receiving responses
        let (tx_res, mut rx_res) = mpsc::channel(10);
        // Channel for retry requests
        let (retry_tx, _retry_rx) = mpsc::channel(10);
        // Initialize DownloadManager with concurrency limit, response transmitter, and retry sender
        let manager = DownloadManager::new(4, tx_res, retry_tx);

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
            let response = timeout(Duration::from_secs(2), rx_res.recv())
                .await
                .expect("timed out")
                .expect("response channel closed");
            assert_eq!(response.status, 200);
            assert_eq!(
                response.request.url.as_str(),
                &format!("{}/test", server.base_url())
            );
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
        let mwp = DownloadMiddlewareProcessor::new();
        let mwp = Arc::new(Mutex::new(mwp));
        let (retry_tx, _retry_rx) = mpsc::channel(1);
        let downloader = Downloader::new(
            client,
            mwp,
            Arc::new(Mutex::new(mpsc::channel(1).1)),
            mpsc::channel(1).0,
            retry_tx,
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
        let mwp = DownloadMiddlewareProcessor::new();
        let mwp = Arc::new(Mutex::new(mwp));
        let (retry_tx, mut _retry_rx) = mpsc::channel(1);
        let mut downloader = Downloader::new(
            Client::new(),
            mwp,
            Arc::new(Mutex::new(rx_req)),
            tx_res,
            retry_tx,
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

        // Receive result: Expect a timeout (no message sent)
        match timeout(Duration::from_millis(500), rx_res.recv()).await {
            Ok(Some(_)) => panic!("Expected no message on response channel for error"),
            _ => {} // Ok: no message received
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
        let mwp = DownloadMiddlewareProcessor::new();
        let mwp = Arc::new(Mutex::new(mwp));
        let (retry_tx, _retry_rx) = mpsc::channel(1);
        let mut downloader = Downloader::new(
            Client::new(),
            mwp,
            Arc::new(Mutex::new(rx_req)),
            tx_res,
            retry_tx,
            Arc::new(AtomicUsize::new(0)),
            Arc::new(AtomicUsize::new(0)),
        );

        tokio::spawn(async move {
            downloader.handle_incoming_requests().await;
        });

        tx_req.send(request.clone()).await.unwrap();
        drop(tx_req);

        if let Ok(Some(response)) = timeout(Duration::from_secs(1), rx_res.recv()).await {
            assert_eq!(response.request.url, request.url);
            let body = response.text;
            assert_eq!(body, "data");
        } else {
            panic!("Expected a Response");
        }
    }

    #[tokio::test]
    async fn test_download_manager_is_idle() {
        let (tx_res, _rx_res) = mpsc::channel(10);
        let (retry_tx, _retry_rx) = mpsc::channel(10);
        let manager = DownloadManager::new(2, tx_res, retry_tx);

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
        let (retry_tx, _retry_rx) = mpsc::channel(10);
        let manager = DownloadManager::new(2, tx_res, retry_tx);

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
            let response = timeout(Duration::from_secs(2), rx_res.recv())
                .await
                .expect("timeout")
                .expect("channel closed");
            assert_eq!(response.status, 200);
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
        let (retry_tx, _retry_rx) = mpsc::channel(10);
        let mut manager = DownloadManager::new(2, tx_res, retry_tx);

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

    #[tokio::test]
    async fn downloader_drops_request_after_retry_threshold() {
        let server = MockServer::start();
        let _ = server.mock(|when, then| {
            when.method(GET).path("/flip");
            then.status(500);
        });

        // Response & Retry channels
        let (tx_res, rx_res) = mpsc::channel::<Response>(10);
        let (retry_tx, mut retry_rx) = mpsc::channel::<Request>(10);

        // Manager with new signature
        let manager = DownloadManager::new(2, tx_res, retry_tx);

        // Initial request
        let req = Request::new(
            Url::parse(&format!("{}/flip", server.base_url())).unwrap(),
            Method::GET,
            HeaderMap::new(),
            Body::default(),
            dummy_callback,
            0,
            0,
            false,
        );
        manager.enqueue_request(req).await;

        // decreasing this to 0..2 will cause the test to fai
        // also increasing to 0..4 will cause a time error as the retry can't be retried further
        for _ in 0..3 {
            let retry_req = timeout(Duration::from_secs(1), retry_rx.recv())
                .await
                .expect("timed out waiting for retry")
                .expect("retry channel closed unexpectedly");
            manager.enqueue_request(retry_req).await;
        }

        tokio::time::sleep(Duration::from_millis(150)).await;
        assert!(retry_rx.is_empty(), "retry queue should be empty");
        assert!(rx_res.is_empty(), "response queue should be empty");
    }

    #[tokio::test]
    async fn download_manager_retries_then_succeeds_and_retry_queue_not_empty() {
        let server = MockServer::start();
        let mut mock = server.mock(|when, then| {
            when.method(GET).path("/flip");
            then.status(500);
        });

        // Response & Retry channels
        let (tx_res, mut rx_res) = mpsc::channel::<Response>(10);
        let (retry_tx, mut retry_rx) = mpsc::channel::<Request>(10);

        // Manager with new signature
        let manager = DownloadManager::new(2, tx_res, retry_tx);

        // Initial request
        let req = Request::new(
            Url::parse(&format!("{}/flip", server.base_url())).unwrap(),
            Method::GET,
            HeaderMap::new(),
            Body::default(),
            dummy_callback,
            0,
            0,
            false,
        );

        manager.enqueue_request(req.clone()).await;

        // We should see a retry get enqueued after the first 500
        let retry_req = timeout(Duration::from_secs(1), retry_rx.recv())
            .await
            .expect("timed out waiting for retry")
            .expect("retry channel closed unexpectedly");

        // delete response with 500
        mock.delete();

        let _ = server.mock(|when, then| {
            when.method(GET).path("/flip");
            then.status(200).body("eventual ok");
        });

        // Feed the retry back to the manager (like the engine would)
        manager.enqueue_request(retry_req).await;

        // Now we should eventually get a successful Response
        let response = timeout(Duration::from_secs(2), rx_res.recv())
            .await
            .expect("timed out waiting for response")
            .expect("response channel closed");
        assert_eq!(response.status.as_u16(), 200);
        assert_eq!(
            response.request.url.as_str(),
            &format!("{}/flip", server.base_url())
        );
        assert_eq!(response.text, "eventual ok");
    }
}
