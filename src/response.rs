use crate::downloader::DownloadResult;
use crate::request::Request;
use bytes::Bytes;
use libxml::parser::Parser;
use libxml::tree::{Document, Node};
use libxml::xpath::Context;
use reqwest::header::HeaderMap;
use std::sync::Arc;

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

impl Response {
    /// Run an XPath query on this Response's HTML, returning a Selector for chaining.
    pub fn xpath(&self, expr: &str) -> SelectorList {
        Selector::from_response(self).xpath(expr)
    }
}

/// A selector over a parsed HTML document supporting chained XPath queries.
pub struct Selector {
    doc: Arc<Document>,
    node: Node,
}

pub struct SelectorList {
    selectors: Vec<Selector>,
}

impl Selector {
    /// Parse the Response into a root Selector
    pub fn from_response(resp: &Response) -> Selector {
        let parser = Parser::default_html();
        let doc = parser
            .parse_string(&resp.text)
            .expect("Failed to parse HTML response");
        let doc = Arc::new(doc);
        let root = doc.get_root_element().expect("No root element in HTML");
        Selector { doc, node: root }
    }

    /// Evaluate XPath on this single node
    pub fn xpath(&self, expr: &str) -> SelectorList {
        let mut ctx = Context::new(&self.doc).expect("Failed to create context from document");
        ctx.set_context_node(&self.node)
            .expect("Failed to set context node");

        let result = ctx
            .evaluate(expr)
            .expect("Failed to evaluate XPath expression");
        let nodes = result.get_nodes_as_vec();

        let selectors = nodes
            .into_iter()
            .map(|n| Selector {
                doc: Arc::clone(&self.doc),
                node: n,
            })
            .collect();

        SelectorList { selectors }
    }

    /// Extract text from this single node
    pub fn extract(&self) -> String {
        self.node.get_content()
    }
}

impl SelectorList {
    /// Apply xpath to all selectors and flatten results
    pub fn xpath(&self, expr: &str) -> SelectorList {
        let mut new_selectors = Vec::new();
        for sel in &self.selectors {
            let sub_list = sel.xpath(expr);
            new_selectors.extend(sub_list.selectors);
        }
        SelectorList {
            selectors: new_selectors,
        }
    }

    /// Extract all text nodes from all selectors
    pub fn extract(&self) -> Vec<String> {
        self.selectors.iter().map(|s| s.extract()).collect()
    }

    /// Borrowing iterator over selectors.
    pub fn iter(&self) -> std::slice::Iter<'_, Selector> {
        self.selectors.iter()
    }
}

// Allows consuming iteration: `for sel in selector_list { ... }`
impl IntoIterator for SelectorList {
    type Item = Selector;
    type IntoIter = std::vec::IntoIter<Selector>;

    fn into_iter(self) -> Self::IntoIter {
        self.selectors.into_iter()
    }
}

// Allows borrowing iteration: `for sel in &selector_list { ... }`
impl<'a> IntoIterator for &'a SelectorList {
    type Item = &'a Selector;
    type IntoIter = std::slice::Iter<'a, Selector>;

    fn into_iter(self) -> Self::IntoIter {
        self.selectors.iter()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::downloader::DownloadResult;
    use crate::request::Request;
    use bytes::Bytes;
    use http::Response as HttpResponse;
    use http::Version;
    use reqwest::Response as ReqwestResponse;
    use reqwest::header::HeaderMap;

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

    #[test]
    fn test_xpath_single_element() {
        let html = r#"<html><body><div class="quote">Hello World</div></body></html>"#;
        let response = Response {
            request: Request::default(),
            status: reqwest::StatusCode::OK,
            headers: HeaderMap::new(),
            body: Bytes::from(html.as_bytes().to_vec()),
            text: html.to_string(),
        };

        let selector_list = response.xpath("//div[@class='quote']");
        let extracted = selector_list.extract();
        assert_eq!(extracted.len(), 1);
        assert_eq!(extracted[0], "Hello World");
    }

    #[test]
    fn test_xpath_multiple_elements() {
        let html = r#"<html><body>
            <div class="quote">Quote 1</div>
            <div class="quote">Quote 2</div>
            <div class="quote">Quote 3</div>
        </body></html>"#;
        let response = Response {
            request: Request::default(),
            status: reqwest::StatusCode::OK,
            headers: HeaderMap::new(),
            body: Bytes::from(html.as_bytes().to_vec()),
            text: html.to_string(),
        };

        let selector_list = response.xpath("//div[@class='quote']");
        let extracted = selector_list.extract();
        assert_eq!(extracted.len(), 3);
        assert_eq!(extracted, vec!["Quote 1", "Quote 2", "Quote 3"]);
    }

    #[test]
    fn test_xpath_chained_nested_elements() {
        let html = r#"<html><body>
            <ul>
                <li><span class="item">Item 1</span></li>
                <li><span class="item">Item 2</span></li>
            </ul>
        </body></html>"#;
        let response = Response {
            request: Request::default(),
            status: reqwest::StatusCode::OK,
            headers: HeaderMap::new(),
            body: Bytes::from(html.as_bytes().to_vec()),
            text: html.to_string(),
        };

        // Select list items first
        let li_selectors = response.xpath("//ul/li");
        assert_eq!(li_selectors.extract().len(), 2);

        // Chain to nested span elements
        let span_selectors = li_selectors.xpath(".//span[@class='item']");
        let extracted = span_selectors.extract();
        assert_eq!(extracted.len(), 2);
        assert_eq!(extracted, vec!["Item 1", "Item 2"]);
    }
}
