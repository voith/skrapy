

# 🕷️ Skrapy

**Skrapy** is an experimental, async web scraping framework written in Rust — inspired by Python's [Scrapy](https://scrapy.org). It's built using `tokio` and serves as a personal learning project to explore Rust async, task scheduling, and modular crawling architectures.

> 🚧 Work In Progress — Only the `requests` module is implemented currently.

---

## ✨ Goals

- Learn and apply Rust async programming with `tokio`
- Design an idiomatic Rust crate inspired by Scrapy
- Explore how to build an event-driven spider engine
- Modular components: `Request`, `Response`, `Spider`, `Item`, etc.

---

## ✅ Current Status

### Implemented:

- `Request` struct with:
  - Method, URL, Headers, Body
  - Canonicalization and fingerprinting
  - Equality based on fingerprint
  - Optional callback: `fn(Response) -> SpiderOutput`

- `Spider` trait:
  - `start_urls() -> Vec<Request>`
  - `parse(response: Response) -> SpiderOutput`

- `SpiderOutput`:
  - Wraps either a list of `Item`s or new `Request`s

---

## 🚧 Coming Next

- `Downloader`: uses `reqwest` to fetch `Request`s
- `Scheduler`: manages request queue and deduplication
- `Engine`: orchestrates crawling lifecycle
- Async task handling with `tokio`

---

## 🧠 Design Inspirations

- **Scrapy**: callback-driven, pluggable spider pipelines
- **Rust**: strong typing, safety, zero-cost abstractions
- **Tokio**: lightweight async tasks, channels, and timers

---

## 📦 Example Usage (future)

```rust
struct MySpider;

impl Spider for MySpider {
    fn start_urls(&self) -> Vec<Request> {
        vec![
            Request::new("https://example.com")
                .with_callback(Self::parse)
        ]
    }

    fn parse(response: Response) -> SpiderOutput {
        let item = Item::new("title", response.text());
        SpiderOutput::from(vec![item])
    }
}
```

---

## 🛠️ Dev Setup

```bash
git clone https://github.com/yourname/skrapy
cd skrapy
cargo build
cargo test
```

---

## 📚 Learning Notes

This project is a sandbox to:
- Practice Rust's ownership model in async contexts
- Understand channels, `Arc`, and lifetimes with task queues
- Explore trait-based plugin systems

---

## 🙏 Acknowledgments

- [Scrapy](https://github.com/scrapy/scrapy) — the original inspiration
- Rust async ecosystem: `tokio`, `reqwest`, `serde`, `sha1`

---

## 📍 License

MIT