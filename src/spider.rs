use crate::{item_pipeline::Pipeline, items::ItemBox, request::Request};

pub type CallbackReturn = Box<dyn Iterator<Item = SpiderOutput> + Send>;

pub enum SpiderOutput {
    Item(ItemBox),
    Request(Request),
}

pub struct SpiderSettings {
    pub pipelines: Vec<Box<dyn Pipeline>>,
}

pub trait Spider {
    fn start(&self) -> CallbackReturn;
    fn settings(&self) -> SpiderSettings {
        SpiderSettings {
            pipelines: Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::request::Request;
    use crate::response::Response;
    use bytes::Bytes;
    use reqwest::{Body, Method, header::HeaderMap};
    use url::Url;

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
            Box::new(vec![SpiderOutput::Request(request)].into_iter())
        }
    }

    impl Spider for MySpider {
        fn start(&self) -> CallbackReturn {
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
            Box::new(vec![SpiderOutput::Request(req1), SpiderOutput::Request(req2)].into_iter())
        }
    }

    #[test]
    fn test_spider_callback_execution() {
        let spider = MySpider;
        let mut requests: Vec<SpiderOutput> = spider.start().collect();
        assert_eq!(requests.len(), 2);
        let http_response = http::Response::builder()
            .status(200)
            .body(reqwest::Body::from("dummy"))
            .unwrap();
        let reqwest_response = reqwest::Response::from(http_response);
        let dummy_response = Response {
            request: Request::default(),
            status: reqwest_response.status(),
            headers: reqwest_response.headers().clone(),
            body: Bytes::new(),
            text: "".to_string(),
        };
        if let SpiderOutput::Request(request) = requests.remove(0) {
            let callback = request.callback;
            let results: Vec<SpiderOutput> = callback(dummy_response).collect();
            assert_eq!(results.len(), 1);
            match &results[0] {
                SpiderOutput::Request(req) => {
                    assert_eq!(req.url.as_str(), "https://example.com/parsed");
                }
                _ => panic!("Expected a Request variant"),
            }
        } else {
            panic!("Expected a Spider::Request");
        }
    }
}
