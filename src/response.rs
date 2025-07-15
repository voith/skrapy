use crate::request::Request;
use reqwest::Response as ReqwestResponse;

pub struct Response {
    pub request: Request,
    pub res: ReqwestResponse,
}
