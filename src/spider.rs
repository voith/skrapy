use crate::{items::Item, request::Request, response::Response};

pub struct SpiderOutput {
    item: Option<Item>,
    request: Option<Request>,
}

pub trait Spider {
    fn start(&self) -> Box<dyn Iterator<Item = Request> + Send>;
}


#[cfg(test)]
mod tests {
    use super::*;
    use url::Url;
    use reqwest::{Method, header::HeaderMap, Body};
    use crate::response::Response;
    use crate::request::Request;
    use crate::items::Item;

    struct MySpider;

    impl MySpider {
        fn parse(_response: Response) -> Box<dyn Iterator<Item = SpiderOutput> + Send> {
            let request = Request::new(
                Url::parse("https://example.com/parsed").unwrap(),
                Method::GET,
                HeaderMap::new(),
                Body::default(),
                Self::parse,
                0,
                0,
                false,
            );
            Box::new(vec![SpiderOutput {
                item: None,
                request: Some(request),
            }].into_iter())
        }
    }

    impl Spider for MySpider {
        fn start(&self) -> Box<dyn Iterator<Item = Request> + Send> {
            let req1 = Request::new(
                Url::parse("https://example.com/1").unwrap(),
                Method::GET,
                HeaderMap::new(),
                Body::default(),
                Self::parse,
                0,
                0,
                true,
            );
            let req2 = Request::new(
                Url::parse("https://example.com/2").unwrap(),
                Method::GET,
                HeaderMap::new(),
                Body::default(),
                Self::parse,
                0,
                0,
                true,
            );
            Box::new(vec![req1, req2].into_iter())
        }
    }

    #[test]
    fn test_spider_callback_execution() {
        let spider = MySpider;
        let mut requests: Vec<Request> = spider.start().collect();
        assert_eq!(requests.len(), 2);

        // Create a dummy Response. This assumes Response::dummy() exists or use Response::default() if available
        let dummy_response = Response::default();
        let callback = requests.remove(0).callback;

        let results: Vec<SpiderOutput> = callback(dummy_response).collect();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].request.as_ref().unwrap().url.as_str(), "https://example.com/parsed");
    }
}
