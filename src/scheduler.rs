use std::collections::{HashSet, BinaryHeap};
use std::cmp::Ordering;
use crate::requests::{Request};

#[derive(Debug, Eq, PartialEq)]
struct PrioritizedRequest {
    priority: i32,
    request: Request,
}

impl Ord for PrioritizedRequest {
    fn cmp(&self, other: &Self) -> Ordering {
        // Lower values should be treated as higher priority
        other.priority.cmp(&self.priority)
    }
}

impl PartialOrd for PrioritizedRequest {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

pub struct PriorityQueue {
    heap: BinaryHeap<PrioritizedRequest>,
}

impl PriorityQueue {
    pub fn new() -> Self {
        PriorityQueue {
            heap: BinaryHeap::new(),
        }
    }

    pub fn push(&mut self, request: Request) {
        let mut base_priority = request.priority(); // 0 is highest priority
        let priority = if request.is_start_request() {
            0 // Make start requests come first
        } else {
            if base_priority == 0 {
                base_priority += 1;
            }
            base_priority
        };

        self.heap.push(PrioritizedRequest { priority, request });
    }

    pub fn pop(&mut self) -> Option<Request> {
        self.heap.pop().map(|entry| entry.request)
    }

    pub fn peek(&self) -> Option<&Request> {
        self.heap.peek().map(|entry| &entry.request)
    }

    pub fn is_empty(&self) -> bool {
        self.heap.is_empty()
    }

    pub fn len(&self) -> usize {
        self.heap.len()
    }
}


pub trait DupeFilter {
    fn record_request_if_unseen(&mut self, request: &Request) -> bool;
}

struct MemoryDupeFilter {
    fingerprints: HashSet<Vec<u8>>
}

struct Scheduler {
    dupefilter: MemoryDupeFilter,
    queue: PriorityQueue,
}


impl MemoryDupeFilter {
    fn new() -> Self {
        Self {
            fingerprints: HashSet::new(),
        }
    }
}

impl DupeFilter for MemoryDupeFilter {
    fn record_request_if_unseen(&mut self, request: &Request) -> bool {
        let fp = request.fingerprint();
        self.fingerprints.insert(fp)
    }
}

impl Scheduler {
    pub fn new() -> Self {
        Scheduler {
            dupefilter: MemoryDupeFilter::new(),
            queue: PriorityQueue::new(),
        }
    }

    pub fn enqueue_request(&mut self, request: Request) -> bool {
        if self.dupefilter.record_request_if_unseen(&request) {
            self.queue.push(request);
            true
        } else {
            false
        }
    }

    pub fn next_request(&mut self) -> Option<Request> {
        self.queue.pop()
    }
}


#[cfg(test)]
mod dupefilter_tests {
    use super::*;
    use crate::requests::{Request};
    use crate::requests::{Method, HeaderMap};
    use reqwest::Url;


    fn dummy_request(url: &str) -> Request {
        Request::new(
            Url::parse(url).unwrap(),
            Method::GET,
            HeaderMap::new(),
            "".into(),
            None,
            0,
            0,
            false,
        )
    }

    #[test]
    fn test_dupefilter_detects_duplicates() {
        let mut filter = MemoryDupeFilter::new();

        let req1 = dummy_request("http://example.com/page");
        let req2 = dummy_request("http://example.com/page");
        let req3 = dummy_request("http://example.com/other");

        assert!(filter.record_request_if_unseen(&req1));
        assert!(!filter.record_request_if_unseen(&req2)); // duplicate
        assert!(filter.record_request_if_unseen(&req3));  // new
    }
}


// PriorityQueue tests
#[cfg(test)]
mod priority_queue_tests {
    use super::*;
    use crate::requests::{Method, HeaderMap};
    use reqwest::Url;

    fn dummy_request_with_priority(url: &str, priority: i32, is_start: bool) -> Request {
        Request::new(
            Url::parse(url).unwrap(),
            Method::GET,
            HeaderMap::new(),
            "".into(),
            None,
            priority,
            0,
            is_start,
        )
    }

    #[test]
    fn test_priority_queue_push_and_pop() {
        let mut queue = PriorityQueue::new();

        let r1 = dummy_request_with_priority("http://example.com/low", 50, false);
        let r2 = dummy_request_with_priority("http://example.com/high", 0, false);
        let r3 = dummy_request_with_priority("http://example.com/medium", 10, false);

        queue.push(r1);
        queue.push(r2);
        queue.push(r3);

        let popped1 = queue.pop().unwrap();
        let popped2 = queue.pop().unwrap();
        let popped3 = queue.pop().unwrap();

        assert_eq!(popped1.url.as_str(), "http://example.com/high");   // priority 0
        assert_eq!(popped2.url.as_str(), "http://example.com/medium"); // priority 10
        assert_eq!(popped3.url.as_str(), "http://example.com/low");    // priority 50
    }

    #[test]
    fn test_priority_queue_respects_start_request() {
        let mut queue = PriorityQueue::new();

        let r1 = dummy_request_with_priority("http://example.com/start", 50, true);
        let r2 = dummy_request_with_priority("http://example.com/normal", 0, false);

        queue.push(r2);
        queue.push(r1);

        let popped = queue.pop().unwrap();
        assert_eq!(popped.url.as_str(), "http://example.com/start");
    }

    #[test]
    fn test_priority_queue_peek() {
        let mut queue = PriorityQueue::new();

        let r1 = dummy_request_with_priority("http://example.com/first", 5, false);
        let r2 = dummy_request_with_priority("http://example.com/second", 3, false);

        queue.push(r1);
        queue.push(r2);

        let peeked = queue.peek().unwrap();
        assert_eq!(peeked.url.as_str(), "http://example.com/second"); // priority 3

        let popped = queue.pop().unwrap();
        assert_eq!(popped.url.as_str(), "http://example.com/second");
    }

    #[test]
    fn test_priority_queue_len_and_empty() {
        let mut queue = PriorityQueue::new();
        assert!(queue.is_empty());

        let r1 = dummy_request_with_priority("http://example.com", 2, false);
        queue.push(r1);
        assert_eq!(queue.len(), 1);
        assert!(!queue.is_empty());

        queue.pop();
        assert!(queue.is_empty());
    }
}
