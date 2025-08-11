//! Downloader error middleware system (Scrapy-style)
//!
//! - `DownloadDecision` has only two states:
//!     - `Continue(DownloadError)`: retry with (possibly modified) error; next middleware still runs
//!     - `Drop`: stop processing and drop the request (short-circuit)
//! - `DownloadMiddlewareProcessor` threads the `DownloadError` through all middlewares in order.
//!   Any `Drop` stops the chain; if no middlewares are present, we default to dropping (no retry).

use crate::downloader::DownloadError;
use crate::request::Request;

/// Decision taken by a download-error middleware.

pub enum DownloadDecision {
    /// Retry this (possibly modified) download error; the processor will continue
    /// to the next middleware with this new `DownloadError`.
    Continue(DownloadError),
    /// Drop this request from the pipeline; short-circuits the chain.
    Drop(DownloadError),
}

/// Scrapy-like download error middleware interface.
pub trait DownloadMiddleware: Send + Sync {
    /// Inspect a failed `DownloadError` and decide what to do.
    ///
    /// Return one of:
    /// - `DownloadDecision::Continue(err)` to retry (the processor will keep chaining)
    /// - `DownloadDecision::Drop` to drop the request (chain stops)
    fn process_error(&self, err: DownloadError) -> DownloadDecision {
        // Default middleware is a no-op that allows retry to proceed.
        DownloadDecision::Continue(err)
    }
}

/// Chains multiple download middlewares in order.
pub struct DownloadMiddlewareProcessor {
    middlewares: Vec<Box<dyn DownloadMiddleware>>,
}

impl DownloadMiddlewareProcessor {
    pub fn new() -> Self {
        Self {
            middlewares: Vec::new(),
        }
    }

    pub fn add<M: DownloadMiddleware + 'static>(&mut self, mw: M) {
        self.middlewares.push(Box::new(mw));
    }

    /// Run the error through all middlewares in order, threading the `DownloadError`.
    ///
    /// Returns `Some(Request)` if the chain completed without a `Drop` (retry this request),
    /// or `None` if the request was dropped or if there were no middlewares.
    pub fn process_error(&self, mut de: DownloadError) -> Option<Request> {
        for mw in &self.middlewares {
            match mw.process_error(de) {
                DownloadDecision::Continue(next) => {
                    de = next;
                }
                DownloadDecision::Drop(next) => {
                    log::error!(
                        "Dropping {} due to download error: {:?}",
                        &next.request,
                        &next.error.source
                    );
                    return None;
                }
            }
        }
        Some(de.request)
    }
}

/// Retry middleware: on error, increment retry_count and retry until it exceeds `max_retries`.
/// Once `request.retry_count()` is greater than `max_retries`, the request is dropped.
pub struct RetryOnErrorMiddleware {
    max_retries: u32,
}

impl RetryOnErrorMiddleware {
    pub fn new(max_retries: u32) -> Self {
        Self { max_retries }
    }
}

impl DownloadMiddleware for RetryOnErrorMiddleware {
    fn process_error(&self, mut de: DownloadError) -> DownloadDecision {
        let current = de.request.retry_count() as u32;
        // retry count starts from 0
        if current > self.max_retries - 1 {
            DownloadDecision::Drop(de)
        } else {
            de.request.increment_retry_count();
            DownloadDecision::Continue(de)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::downloader::HttpError;
    use crate::request::Request;
    use tokio::io::AsyncWriteExt;
    use tokio::net::TcpListener;
    use tokio::task::JoinHandle;

    // Start a tiny mock server that always returns 500. No extra deps.
    async fn start_mock_server() -> (String, JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let handle = tokio::spawn(async move {
            loop {
                match listener.accept().await {
                    Ok((mut socket, _)) => {
                        // Minimal valid HTTP/1.1 500 response
                        let _ = socket
                            .write_all(b"HTTP/1.1 500 Internal Server Error\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                            .await;
                        let _ = socket.shutdown().await;
                    }
                    Err(_) => break,
                }
            }
        });
        (format!("http://{}", addr), handle)
    }

    // Build a DownloadError by actually hitting the mock server and turning 500 into a reqwest::Error.
    async fn mk_err(req: Request, base: &str) -> DownloadError {
        let url = format!("{}/", base.trim_end_matches('/'));
        let resp = reqwest::Client::new().get(&url).send().await.unwrap();
        let source = resp.error_for_status().unwrap_err();
        DownloadError {
            request: req,
            error: HttpError { source },
        }
    }

    #[tokio::test]
    async fn processor_returns_request_when_no_middleware_retries() {
        let (base, server) = start_mock_server().await;
        let url = format!("{}/", &base);
        let proc = DownloadMiddlewareProcessor::new();
        let req = Request::get(&url);
        let out = proc.process_error(mk_err(req, &base).await);
        assert!(
            out.is_some(),
            "Expected Some(Request) when there are no middlewares"
        );
        server.abort();
    }

    #[tokio::test]
    async fn retry_middleware_retries_until_exceeds_threshold() {
        let (base, server) = start_mock_server().await;
        let url = format!("{}/", &base);
        let mw = RetryOnErrorMiddleware::new(4);
        let req = Request::get(&url);
        let mut de = mk_err(req, &base).await;

        // First call: current=0 → increment to 1 and continue
        match mw.process_error(de) {
            DownloadDecision::Continue(next) => {
                assert_eq!(next.request.retry_count(), 1);
                de = next;
            }
            DownloadDecision::Drop(_) => {
                panic!("expected Continue on first retry, got Drop instead")
            }
        }
        // Second call: current=1 → increment to 2 and continue
        match mw.process_error(de) {
            DownloadDecision::Continue(next) => {
                assert_eq!(next.request.retry_count(), 2);
                de = next;
            }
            _ => panic!("expected Continue on second retry, got Drop instead"),
        }
        // Third call: current=2 → increment to 3 and continue
        match mw.process_error(de) {
            DownloadDecision::Continue(next) => {
                assert_eq!(next.request.retry_count(), 3);
                de = next;
            }
            _ => panic!("expected Continue on third retry, got Drop instead"),
        }
        // Fourth call: current=3 → increment to 4 and continue
        match mw.process_error(de) {
            DownloadDecision::Continue(next) => {
                assert_eq!(next.request.retry_count(), 4);
                de = next;
            }
            _ => panic!("expected Continue on fourth retry, got Drop instead"),
        }
        // Fifth call: current=4 → Drop (> max_retries)
        match mw.process_error(de) {
            DownloadDecision::Drop(_) => {}
            _ => panic!("expected Drop, got Drop instead"),
        }
        server.abort();
    }

    struct DropMw;
    impl DownloadMiddleware for DropMw {
        fn process_error(&self, de: DownloadError) -> DownloadDecision {
            DownloadDecision::Drop(de)
        }
    }

    struct ContinueMw;
    impl DownloadMiddleware for ContinueMw {
        fn process_error(&self, mut de: DownloadError) -> DownloadDecision {
            de.request.increment_retry_count();
            DownloadDecision::Continue(de)
        }
    }

    #[tokio::test]
    async fn processor_chains_multiple_continue_middlewares() {
        let (base, server) = start_mock_server().await;
        let url = format!("{}/", &base);
        let mut proc = DownloadMiddlewareProcessor::new();
        proc.add(ContinueMw); // +1
        proc.add(ContinueMw); // +1
        let req = Request::get(&url);
        let out = proc
            .process_error(mk_err(req, &base).await)
            .expect("should retry");
        assert_eq!(out.retry_count(), 2, "both middlewares should have run");
        server.abort();
    }

    #[tokio::test]
    async fn processor_short_circuits_on_drop() {
        let (base, server) = start_mock_server().await;
        let url = format!("{}/", &base);
        let mut proc = DownloadMiddlewareProcessor::new();
        proc.add(DropMw);
        proc.add(ContinueMw); // should never run
        let req = Request::get(&url);
        let out = proc.process_error(mk_err(req, &base).await);
        assert!(out.is_none(), "Expected Drop to short-circuit the chain");
        server.abort();
    }

    #[tokio::test]
    async fn processor_with_retry_middleware_drops_on_third_attempt() {
        // Configure retry middleware to allow up to 2 retries.
        // With starting retry_count=0: attempt1 -> cnt=1 (continue), attempt2 -> cnt=2 (continue),
        // attempt3 sees current=2 > max(2) -> Drop → processor returns None.
        let (base, server) = start_mock_server().await;
        let url = format!("{}/", &base);

        let mut proc = DownloadMiddlewareProcessor::new();
        proc.add(RetryOnErrorMiddleware::new(2));

        // Attempt 1
        let req = Request::get(&url);
        let out1 = proc.process_error(mk_err(req, &base).await);
        let req = out1.expect("attempt 1 should continue");
        assert_eq!(req.retry_count(), 1);

        // Attempt 2
        let out2 = proc.process_error(mk_err(req, &base).await);
        let req = out2.expect("attempt 2 should continue");
        assert_eq!(req.retry_count(), 2);

        // Attempt 3 -> should drop (None)
        let out3 = proc.process_error(mk_err(req, &base).await);
        assert!(out3.is_none(), "attempt 3 should drop and return None");

        server.abort();
    }
}
