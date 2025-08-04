

# 🕷️ Skrapy

**Skrapy** is an experimental, async web scraping framework written in Rust — inspired by Python's [Scrapy](https://scrapy.org). It's built using `tokio` and serves as a personal learning project to explore Rust async, task scheduling, and modular crawling architectures.

---

## ✨ Goals

- Learn and apply Rust async programming with `tokio`
- Design an idiomatic Rust crate inspired by Scrapy
- Explore how to build an event-driven spider engine
- Modular components: `Request`, `Response`, `Spider`, `Item`, etc.

---

## 🧠 Design Inspirations

- **Scrapy**: the original inspiration for the callback-driven spider and pipelines
- **Tokio**: for async runtime and task scheduling
- **Reqwest**: for async HTTP requests in the downloader

---

## 📦 Example Usage

Below is a working example of a simple spider scraping [Quotes to Scrape](https://quotes.toscrape.com/):

```rust
use serde_json::json;
use skrapy::{
    engine::Engine,
    item_pipeline::JsonLinesExporter,
    items::ItemBox,
    request::Request,
    response::Response,
    spider::{CallbackReturn, Spider, SpiderOutput, SpiderSettings},
};
use std::env;
use std::iter::once;
use std::path::PathBuf;
use url::Url;

fn join_url(base_url: &str, url: &str) -> String {
    let base_url = Url::parse(base_url).unwrap();
    base_url.join(url).unwrap().into()
}

struct QuotesSpider {
    base_url: String,
}

impl Spider for QuotesSpider {
    fn settings(&self) -> SpiderSettings {
        // Create a JsonLinesExporter writing to "./quotes.json" in the current working directory
        let file_path: PathBuf = env::current_dir()
            .expect("failed to get current directory")
            .join("quotes.json");
        SpiderSettings {
            pipelines: vec![Box::new(JsonLinesExporter::new(file_path))],
        }
    }

    fn start(&self) -> CallbackReturn {
        let url = join_url(&self.base_url, "/page/1/");
        let req = Request::get(&url).with_callback(Self::parse);
        Box::new(once(SpiderOutput::Request(req)))
    }
}

impl QuotesSpider {
    fn new(base_url: String) -> Self {
        Self { base_url }
    }

    fn parse(response: Response) -> CallbackReturn {
        let quote_list = response.xpath("//div[@class='quote']");
        let mut outputs = Vec::new();
        for quote in quote_list.into_iter() {
            let author = quote
                .xpath("span/small/text()")
                .extract()
                .into_iter()
                .next()
                .unwrap_or_default();
            let text = quote
                .xpath("span[@class='text']/text()")
                .extract()
                .into_iter()
                .next()
                .unwrap_or_default();
            let item: ItemBox = Box::new(json!({ "author": author, "text": text }));
            outputs.push(SpiderOutput::Item(item));
        }

        // Handle pagination
        if let Some(href) = response
            .xpath("//li[@class='next']/a/@href")
            .extract()
            .into_iter()
            .next()
        {
            let next_url = join_url("https://quotes.toscrape.com/", &href);
            let req = Request::get(&next_url).with_callback(Self::parse);
            outputs.push(SpiderOutput::Request(req));
        }

        Box::new(outputs.into_iter())
    }
}

#[tokio::main]
async fn main() {
    let spider = QuotesSpider::new("https://quotes.toscrape.com/".to_string());
    let mut engine = Engine::new(Box::new(spider));
    engine.run().await;
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