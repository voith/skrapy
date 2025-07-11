use crate::response::Response;
use crate::spider::SpiderOutput;
use sha1::{Digest, Sha1};
use std::cmp::PartialEq;
use url::Url as UrlLib;

// Imported here so that users can directly import from skrapy
pub use reqwest::{Body, Method, Url, header::HeaderMap};

#[derive(Debug)]
pub struct Request {
    pub url: Url,
    pub method: Method,
    pub headers: HeaderMap,
    pub body: Body,
    pub callback: Option<fn(Response) -> SpiderOutput>,
    _internal_meta_data: InternalRequestMetaData,
}

#[derive(Debug, Clone)]
struct InternalRequestMetaData {
    priority: i32,
    redirect_count: i8,
    is_start_request: bool,
}

impl Default for Request {
    fn default() -> Self {
        Self {
            url: Url::parse("http://localhost/").unwrap(),
            method: Method::GET,
            headers: HeaderMap::new(),
            body: Body::default(),
            callback: None,
            _internal_meta_data: InternalRequestMetaData {
                priority: 0,
                redirect_count: 0,
                is_start_request: true,
            },
        }
    }
}

impl Request {
    pub fn new(
        url: Url,
        method: Method,
        headers: HeaderMap,
        body: Body,
        callback: Option<fn(Response) -> SpiderOutput>,
        priority: i32,
        redirect_count: i8,
        is_start_request: bool,
    ) -> Self {
        Self {
            url,
            method,
            headers,
            body,
            callback,
            _internal_meta_data: InternalRequestMetaData {
                priority,
                redirect_count,
                is_start_request,
            },
        }
    }

    pub fn fingerprint(&self) -> Vec<u8> {
        let mut hasher = Sha1::new();
        hasher.update(self.method.as_str().as_bytes());

        let canonical =
            canonicalize_url(self.url.as_str()).unwrap_or_else(|| self.url.as_str().to_string());
        hasher.update(canonical.as_bytes());

        if let Some(body) = self.body.as_bytes() {
            hasher.update(body);
        }

        hasher.finalize().to_vec()
    }

    pub fn priority(&self) -> i32 {
        self._internal_meta_data.priority
    }

    pub fn is_start_request(&self) -> bool {
        self._internal_meta_data.is_start_request
    }
}

impl PartialEq for Request {
    fn eq(&self, other: &Self) -> bool {
        self.fingerprint() == other.fingerprint()
    }
}

impl Eq for Request {}

impl Clone for Request {
    fn clone(&self) -> Self {
        // Clone the body by extracting bytes, falling back to empty if needed
        let body_bytes = self.body.as_bytes().unwrap_or(&[]);
        let body_clone = Body::from(body_bytes.to_vec());

        Self {
            url: self.url.clone(),
            method: self.method.clone(),
            headers: self.headers.clone(),
            body: body_clone,
            callback: self.callback,
            _internal_meta_data: self._internal_meta_data.clone(),
        }
    }
}

pub fn canonicalize_url(input: &str) -> Option<String> {
    let mut url = UrlLib::parse(input).ok()?;

    // Lowercase scheme and host
    let scheme = url.scheme().to_lowercase();
    let host = url.host_str()?.to_lowercase();

    url.set_scheme(&scheme).ok()?;
    url.set_host(Some(&host)).ok()?;

    // Remove default ports
    if (url.scheme() == "http" && url.port() == Some(80))
        || (url.scheme() == "https" && url.port() == Some(443))
    {
        url.set_port(None).ok()?;
    }

    // Sort and filter query parameters (remove blank values)
    if let Some(query) = url.query() {
        let mut pairs: Vec<_> = url::form_urlencoded::parse(query.as_bytes())
            .filter(|(_, v)| !v.is_empty())
            .collect();
        pairs.sort();
        let new_query = url::form_urlencoded::Serializer::new(String::new())
            .extend_pairs(pairs)
            .finish();
        url.set_query(Some(&new_query));
    }

    Some(url.to_string())
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn test_canonical_url_sorted_query() {
        let url1 = "http://example.com/path?param2=valueB&param1=valueA";
        let url2 = "http://example.com/path?param1=valueA&param2=valueB";

        let canonical_url1 = canonicalize_url(url1);
        let canonical_url2 = canonicalize_url(url2);

        assert_eq!(canonical_url1, canonical_url2);
    }

    #[test]
    fn test_canonical_url_empty() {
        let url1 = "http://example.com/path?param1=valueA&param2=valueB";
        let url2 = "http://example.com/path?param1=valueA&param2=valueB&empty_param=";

        let canonical_url1 = canonicalize_url(url1);
        let canonical_url2 = canonicalize_url(url2);

        assert_eq!(canonical_url1, canonical_url2);
    }

    #[test]
    fn test_canonical_url_with_mixed_case() {
        let url1 = "HTTP://Example.COM/path?Param=Value";
        let url2 = "http://example.com/path?Param=Value";

        let canonical_url1 = canonicalize_url(url1);
        let canonical_url2 = canonicalize_url(url2);

        assert_eq!(canonical_url1, canonical_url2);
    }

    #[test]
    fn test_canonical_url_no_query() {
        let url = "http://example.com/path";
        let expected = Some("http://example.com/path".to_string());

        assert_eq!(canonicalize_url(url), expected);
    }

    #[test]
    fn test_canonical_url_with_default_ports() {
        let url1 = "http://example.com:80/path?param=value";
        let url2 = "http://example.com/path?param=value";

        let canonical_url1 = canonicalize_url(url1);
        let canonical_url2 = canonicalize_url(url2);

        assert_eq!(canonical_url1, canonical_url2);
    }

    #[test]
    fn test_request_default() {
        let req = Request::default();
        assert_eq!(req.url.as_str(), "http://localhost/");
        assert_eq!(req.method, Method::GET);
        assert!(req.headers.is_empty());
        assert_eq!(req.body.as_bytes().unwrap(), b"");
    }

    #[test]
    fn test_request_fingerprint_consistency() {
        let req1 = Request {
            url: Url::parse("http://example.com/path?b=2&a=1").unwrap(),
            method: Method::POST,
            headers: HeaderMap::new(),
            body: Body::from("hello world"),
            ..Default::default()
        };

        let req2 = Request {
            url: Url::parse("http://example.com/path?a=1&b=2").unwrap(),
            method: Method::POST,
            headers: HeaderMap::new(),
            body: Body::from("hello world"),
            ..Default::default()
        };

        assert_eq!(req1.fingerprint(), req2.fingerprint());
    }

    #[test]
    fn test_request_equality_via_fingerprint() {
        let req1 = Request {
            url: Url::parse("http://example.com/path?a=1&b=2").unwrap(),
            method: Method::POST,
            headers: HeaderMap::new(),
            body: Body::from("data"),
            ..Default::default()
        };

        let req2 = Request {
            url: Url::parse("http://example.com/path?b=2&a=1").unwrap(),
            method: Method::POST,
            headers: HeaderMap::new(),
            body: Body::from("data"),
            ..Default::default()
        };

        assert!(req1 == req2);
    }

    #[test]
    fn test_request_inequality_via_fingerprint() {
        let req1 = Request {
            url: Url::parse("http://example.com/path?a=1&b=2").unwrap(),
            method: Method::POST,
            headers: HeaderMap::new(),
            body: Body::from("data"),
            ..Default::default()
        };

        let req2 = Request {
            url: Url::parse("http://example.com/path?a=1&b=2").unwrap(),
            method: Method::POST,
            headers: HeaderMap::new(),
            body: Body::from("DIFFERENT"),
            ..Default::default()
        };

        assert!(req1 != req2);
    }
}
