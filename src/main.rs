use scraper::{Html, Selector};
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
        let document = Html::parse_document(&response.text);
        // selector for each quote block
        let quote_sel = Selector::parse("div.quote").unwrap();
        // selectors for author and text within a quote
        let author_sel = Selector::parse("span > small").unwrap();
        let text_sel = Selector::parse("span.text").unwrap();

        // collect all items
        let items_iter = document.select(&quote_sel).map(|quote_elem| {
            let author = quote_elem
                .select(&author_sel)
                .next()
                .map(|e| e.text().collect::<String>())
                .unwrap_or_default();
            let text = quote_elem
                .select(&text_sel)
                .next()
                .map(|e| e.text().collect::<String>())
                .unwrap_or_default();
            let item: ItemBox = Box::new(json!({ "author": author, "text": text }));
            SpiderOutput::Item(item)
        });

        // find next page
        let next_sel = Selector::parse("li.next a").unwrap();
        let next_iter = document
            .select(&next_sel)
            .filter_map(|a_elem| a_elem.value().attr("href"))
            .take(1)
            .map(|href| {
                let next_url = join_url("https://quotes.toscrape.com/", href);
                let req = Request::get(&next_url).with_callback(Self::parse);
                SpiderOutput::Request(req)
            });
        let vec_items: Vec<SpiderOutput> = items_iter.chain(next_iter).collect();
        Box::new(vec_items.into_iter())
    }
}

#[tokio::main]
async fn main() {
    let spider = QuotesSpider::new("https://quotes.toscrape.com/".to_string());
    let mut engine = Engine::new(Box::new(spider));
    engine.run().await;
}
