use crate::request::Request;
use reqwest::Response as ReqwestResponse;
use crate::downloader::DownloadResult;

pub struct Response {
    pub request: Request,
    pub res: ReqwestResponse,
}

impl From<DownloadResult> for Response {
    fn from(download_result: DownloadResult) -> Self {
        Response {
            request: download_result.request,
            res: download_result.response,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use http::Response as HttpResponse;
    use reqwest::Response as ReqwestResponse;
    use crate::request::Request;
    use crate::downloader::DownloadResult;
    use http::Version;


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
            response: reqwest_response,
        };

        // Convert DownloadResult into Response
        let response = Response::from(download_result);

        // Assert that the url in the resulting Response matches the input request
        assert_eq!(response.request.url, request.url);
    }
}
