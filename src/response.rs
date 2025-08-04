use crate::downloader::DownloadResult;
use crate::request::Request;
use bytes::Bytes;
use reqwest::header::HeaderMap;

pub struct Response {
    pub request: Request,
    pub status: reqwest::StatusCode,
    pub headers: HeaderMap,
    pub body: Bytes,
    pub text: String,
}

impl From<DownloadResult> for Response {
    fn from(download_result: DownloadResult) -> Self {
        Response {
            request: download_result.request,
            status: download_result.status,
            headers: download_result.headers,
            body: download_result.body,
            text: download_result.text,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::downloader::DownloadResult;
    use crate::request::Request;
    use http::Response as HttpResponse;
    use http::Version;
    use reqwest::Response as ReqwestResponse;

    #[test]
    fn test_response_from_download_result() {
        // Build a dummy HTTP response using http::Response::builder()
        let http_response = HttpResponse::builder()
            .status(200)
            .version(Version::HTTP_11)
            .header("content-type", "text/html")
            .body(Vec::new())
            .unwrap();

        let reqwest_response: ReqwestResponse = ReqwestResponse::from(http_response);

        // Create a dummy Request
        let request = Request::default();

        // Create a DownloadResult
        let download_result = DownloadResult {
            request: request.clone(),
            status: reqwest_response.status(),
            headers: reqwest_response.headers().clone(),
            body: Bytes::new(),
            text: "".to_string(),
        };

        // Convert DownloadResult into Response
        let response = Response::from(download_result);

        // Assert that the url in the resulting Response matches the input request
        assert_eq!(response.request.url, request.url);
    }
}
