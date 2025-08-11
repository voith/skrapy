use crate::response::Response;
use crate::spider::CallbackReturn;
use sha1::{Digest, Sha1};
use std::cmp::PartialEq;
use std::fmt;
use url::Url as UrlLib;

// Imported here so that users can directly import from skrapy
pub use reqwest::{Body, Method, Url, header::HeaderMap};

pub struct Request {
    pub url: Url,
    pub method: Method,
    pub headers: HeaderMap,
    pub body: Body,
    pub callback: fn(Response) -> CallbackReturn,
    _internal_meta_data: InternalRequestMetaData,
}

#[derive(Clone)]
struct InternalRequestMetaData {
    priority: i32,
    retry_count: i8,
    is_start_request: bool,
}

fn is_empty_callback(_: Response) -> CallbackReturn {
    Box::new(std::iter::empty())
}

impl Default for Request {
    fn default() -> Self {
        Self {
            url: Url::parse("http://localhost/").unwrap(),
            method: Method::GET,
            headers: HeaderMap::new(),
            body: Body::default(),
            callback: is_empty_callback,
            _internal_meta_data: InternalRequestMetaData {
                priority: 0,
                retry_count: 0,
                is_start_request: false,
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
        callback: fn(Response) -> CallbackReturn,
        priority: i32,
        retry_count: i8,
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
                retry_count,
                is_start_request,
            },
        }
    }

    pub fn get(url: &str) -> Self {
        Self {
            url: Url::parse(url).unwrap(),
            method: Method::GET,
            headers: HeaderMap::new(),
            body: Body::default(),
            callback: |_| Box::new(std::iter::empty()),
            _internal_meta_data: InternalRequestMetaData {
                priority: 0,
                retry_count: 0,
                is_start_request: false,
            },
        }
    }

    pub fn post(url: &str) -> Self {
        Self {
            url: Url::parse(url).unwrap(),
            method: Method::POST,
            headers: HeaderMap::new(),
            body: Body::default(),
            callback: |_| Box::new(std::iter::empty()),
            _internal_meta_data: InternalRequestMetaData {
                priority: 0,
                retry_count: 0,
                is_start_request: false,
            },
        }
    }

    pub fn with_callback(mut self, cb: fn(Response) -> CallbackReturn) -> Self {
        self.callback = cb;
        self
    }

    pub fn as_start(mut self) -> Self {
        self._internal_meta_data.is_start_request = true;
        self
    }

    pub fn validate(&self) {
        let empty_ptr = is_empty_callback as usize;
        let callback_ptr = self.callback as usize;

        if callback_ptr == empty_ptr {
            // TODO(Voith): return an Error
            panic!("ValidationError: Callback cannot be empty for {}", self.url);
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

    pub fn retry_count(&self) -> i8 {
        self._internal_meta_data.retry_count
    }

    pub fn increment_retry_count(&mut self) {
        self._internal_meta_data.retry_count += 1;
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

impl fmt::Display for Request {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "<{} {}>", &self.method, &self.url)
    }
}

impl std::fmt::Debug for Request {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self) // delegate to your Display impl
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
        if !pairs.is_empty() {
            let new_query = url::form_urlencoded::Serializer::new(String::new())
                .extend_pairs(pairs)
                .finish();
            url.set_query(Some(&new_query));
        } else {
            url.set_query(None);
        }
    }

    Some(url.to_string())
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn test_get_request_with_callback() {
        fn dummy_callback(_: Response) -> CallbackReturn {
            Box::new(std::iter::empty())
        }

        let req = Request::get("http://example.com").with_callback(dummy_callback);

        assert_eq!(req.url.as_str(), "http://example.com/");
        assert_eq!(req.method, Method::GET);
        assert!(req.headers.is_empty());
        assert_eq!(req.body.as_bytes().unwrap(), b"");
        assert_eq!(req.callback as usize, dummy_callback as usize);
    }

    #[test]
    fn test_post_request_with_callback() {
        fn dummy_callback(_: Response) -> CallbackReturn {
            Box::new(std::iter::empty())
        }

        let req = Request::post("http://example.com").with_callback(dummy_callback);

        assert_eq!(req.url.as_str(), "http://example.com/");
        assert_eq!(req.method, Method::POST);
        assert!(req.headers.is_empty());
        assert_eq!(req.body.as_bytes().unwrap(), b"");
        assert_eq!(req.callback as usize, dummy_callback as usize);
    }

    #[test]
    fn test_request_chaining_post_callback() {
        fn dummy_callback(_: Response) -> CallbackReturn {
            Box::new(std::iter::empty())
        }

        let req = Request::post("http://example.com/path").with_callback(dummy_callback);

        assert_eq!(req.url.as_str(), "http://example.com/path");
        assert_eq!(req.method, Method::POST);
        assert_eq!(req.callback as usize, dummy_callback as usize);
    }

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

    #[test]
    fn test_request_as_start() {
        let req = Request::get("http://example.com").as_start();
        assert!(req.is_start_request());
    }

    #[test]
    #[should_panic(expected = "ValidationError: Callback cannot be empty for http://localhost/")]
    fn test_validate_panics_on_empty_callback() {
        let req = Request::default();
        req.validate();
    }

    #[test]
    fn test_increment_retry_count_increments() {
        let mut req = Request::default();
        assert_eq!(req.retry_count(), 0);
        // This test will catch if increment_retry_count does not mutate the original
        req.increment_retry_count();
        assert_eq!(req.retry_count(), 1);
    }

    #[test]
    fn test_validate_ok_with_non_empty_callback() {
        fn cb(_: Response) -> CallbackReturn {
            Box::new(std::iter::empty())
        }
        let req = Request::get("http://example.com").with_callback(cb);
        // Should not panic
        req.validate();
    }

    #[test]
    fn test_is_start_request_default_false() {
        let req = Request::default();
        assert!(!req.is_start_request());
    }

    #[test]
    fn test_priority_from_new() {
        fn cb(_: Response) -> CallbackReturn {
            Box::new(std::iter::empty())
        }
        let req = Request::new(
            Url::parse("http://example.com/").unwrap(),
            Method::GET,
            HeaderMap::new(),
            Body::default(),
            cb,
            42,
            0,
            false,
        );
        assert_eq!(req.priority(), 42);
    }

    #[test]
    fn test_chaining_with_callback_then_as_start() {
        fn cb(_: Response) -> CallbackReturn {
            Box::new(std::iter::empty())
        }
        let req = Request::get("http://example.com")
            .with_callback(cb)
            .as_start();
        assert_eq!(req.callback as usize, cb as usize);
        assert!(req.is_start_request());
    }

    #[test]
    fn test_clone_behavior() {
        fn cb(_: Response) -> CallbackReturn {
            Box::new(std::iter::empty())
        }
        let mut headers = HeaderMap::new();
        headers.insert("X-Test", "1".parse().unwrap());
        let req1 = Request::new(
            Url::parse("http://example.com/path").unwrap(),
            Method::POST,
            headers,
            Body::from("payload"),
            cb,
            0,
            0,
            false,
        );
        let req2 = req1.clone();
        assert_eq!(req1.fingerprint(), req2.fingerprint());
        assert_eq!(req1.body.as_bytes().unwrap(), req2.body.as_bytes().unwrap());
        assert_eq!(req1.callback as usize, req2.callback as usize);
        // headers cloned, not moved
        assert!(req2.headers.get("X-Test").is_some());
    }

    #[test]
    fn test_headers_ignored_in_equality() {
        fn cb(_: Response) -> CallbackReturn {
            Box::new(std::iter::empty())
        }
        let mut h1 = HeaderMap::new();
        h1.insert("A", "1".parse().unwrap());
        let mut h2 = HeaderMap::new();
        h2.insert("B", "2".parse().unwrap());
        let r1 = Request::new(
            Url::parse("http://example.com/").unwrap(),
            Method::GET,
            h1,
            Body::default(),
            cb,
            0,
            0,
            false,
        );
        let r2 = Request::new(
            Url::parse("http://example.com/").unwrap(),
            Method::GET,
            h2,
            Body::default(),
            cb,
            0,
            0,
            false,
        );
        assert_eq!(r1, r2);
    }

    #[test]
    fn test_method_affects_equality() {
        let url = Url::parse("http://example.com/").unwrap();
        let r_get = Request {
            url: url.clone(),
            method: Method::GET,
            ..Default::default()
        };
        let r_post = Request {
            url,
            method: Method::POST,
            ..Default::default()
        };
        assert_ne!(r_get, r_post);
    }

    #[test]
    fn test_body_empty_vs_default_equal() {
        let r1 = Request {
            body: Body::default(),
            ..Default::default()
        };
        let r2 = Request {
            body: Body::from(""),
            ..Default::default()
        };
        assert_eq!(r1, r2);
    }

    #[test]
    fn test_canonical_url_invalid_returns_none() {
        assert_eq!(canonicalize_url("http://exa mple.com"), None);
    }

    #[test]
    fn test_canonical_url_https_default_port_removed() {
        let u1 = canonicalize_url("https://example.com:443/a?b=1");
        let u2 = canonicalize_url("https://example.com/a?b=1");
        assert_eq!(u1, u2);
    }

    #[test]
    fn test_canonical_url_non_default_port_preserved() {
        let u1 = canonicalize_url("http://example.com:8080/a");
        let u2 = canonicalize_url("http://example.com/a");
        assert_ne!(u1, u2);
    }

    #[test]
    fn test_canonical_url_path_case_sensitivity() {
        // host case-insensitive
        let u1 = canonicalize_url("http://EXAMPLE.com/Foo").unwrap();
        let u2 = canonicalize_url("http://example.com/Foo").unwrap();
        assert_eq!(u1, u2);
        // path is case-sensitive
        let u3 = canonicalize_url("http://example.com/Foo").unwrap();
        let u4 = canonicalize_url("http://example.com/foo").unwrap();
        assert_ne!(u3, u4);
    }

    #[test]
    fn test_canonical_url_all_blank_params_removed() {
        let u = canonicalize_url("http://example.com/?a=&b=").unwrap();
        assert_eq!(u, "http://example.com/");
    }

    #[test]
    fn test_display_format_and_debug() {
        let req = Request::get("http://example.com/");
        let disp = format!("{}", req);
        assert_eq!(disp, "<GET http://example.com/>");
        let dbg = format!("{:?}", req);
        assert_eq!(dbg, disp);
    }

    #[test]
    fn test_get_post_defaults_without_callback() {
        let get = Request::get("http://example.com/");
        assert_eq!(get.method, Method::GET);
        assert!(get.headers.is_empty());
        assert_eq!(get.body.as_bytes().unwrap(), b"");
        assert!(!get.is_start_request());
        assert_eq!(get.retry_count(), 0);
        assert_eq!(get.priority(), 0);

        let post = Request::post("http://example.com/");
        assert_eq!(post.method, Method::POST);
        assert!(post.headers.is_empty());
        assert_eq!(post.body.as_bytes().unwrap(), b"");
        assert!(!post.is_start_request());
        assert_eq!(post.retry_count(), 0);
        assert_eq!(post.priority(), 0);
    }

    #[test]
    fn test_fragments_affect_fingerprint_current_behavior() {
        // Current canonicalize_url keeps fragments, so fingerprints should differ
        let r1 = Request {
            url: Url::parse("http://example.com/a#x").unwrap(),
            ..Default::default()
        };
        let r2 = Request {
            url: Url::parse("http://example.com/a#y").unwrap(),
            ..Default::default()
        };
        assert_ne!(r1, r2);
    }
}
