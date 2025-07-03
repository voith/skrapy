use crate::datastructures::{Request};


struct Scheduler {}

impl Scheduler {
    fn enqueue_request(&request: Request) -> bool;
    fn next_request() -> Request;
}
