use crate::{items::Item, requests::Request, response::Response};

pub struct SpiderOutput {
    items: Option<Vec<Item>>,
    requests: Option<Vec<Request>>,
}

impl From<Vec<Item>> for SpiderOutput {
    fn from(items: Vec<Item>) -> Self {
        Self {
            items: Some(items),
            requests: None,
        }
    }
}

impl From<Vec<Request>> for SpiderOutput {
    fn from(requests: Vec<Request>) -> Self {
        Self {
            items: None,
            requests: Some(requests),
        }
    }
}

pub trait Spider {
    fn start_urls(&self) -> Vec<Request>;
    fn parse(&self, response: Response) -> SpiderOutput;
}
