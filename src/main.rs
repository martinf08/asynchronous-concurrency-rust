use async_recursion::async_recursion;
use reqwest::{Client, Request, RequestBuilder};
use select::document::Document;
use select::node::Node;
use select::predicate::*;
use select::selection::Selection;
use std::collections::HashMap;
use std::error::Error;
use std::time::Duration;
use tokio::io::AsyncReadExt;
use tokio::macros::support::thread_rng_n;
use tokio::runtime;
use tokio::sync::mpsc;
use tokio::time::*;

const BASE_URI: &str = "https://www.webscraper.io";

#[tokio::main]
async fn main() {
    let cpu_pool = runtime::Builder::new_multi_thread()
        .enable_time()
        .enable_io()
        .build()
        .unwrap();

    let client = reqwest::Client::new();
    let categories = get_categories(&client).await.unwrap();

    let (tx, mut rx) = mpsc::channel(100);

    for (category, uri) in categories {
        let tx_clone = tx.clone();
        let client_clone = client.clone();

        cpu_pool.spawn(async move {
            dbg!(&category);
            if let Err(_) = tx_clone
                .send(get_product_batches(client_clone, category, uri, 1).await)
                .await
            {
                println!("Receiver dropped");
                return;
            }
        });
    }
    while let Some(products_urls) = rx.recv().await {
        if let Ok(urls) = products_urls {
            dbg!(urls);
        }
    }
}

async fn get_categories(client: &Client) -> Result<HashMap<String, String>, Box<dyn Error>> {
    let res = client
        .get(format!("{}/test-sites/e-commerce/static", self::BASE_URI))
        .send()
        .await?
        .text()
        .await?;

    Ok(Document::from(res.as_str())
        .find(Attr("id", "side-menu").descendant(Name("a")))
        .into_selection()
        .iter()
        .filter(|n| n.attr("href").is_some())
        .map(|n| {
            (
                n.text().trim().to_string(),
                n.attr("href").unwrap().to_string(),
            )
        })
        .collect::<HashMap<String, String>>())
}

#[async_recursion]
async fn get_product_batches(
    client: Client,
    category_name: String,
    uri: String,
    page: u32,
) -> Result<Vec<String>, reqwest::Error> {
    dbg!(&uri);
    let res = client
        .get(format!("{}{}", self::BASE_URI, uri))
        .send()
        .await?
        .text()
        .await?;

    let product_urls = Document::from(res.as_str())
        .find(
            And(Name("div"), Class("row"))
                .descendant(Class("thumbnail"))
                .descendant(Name("a")),
        )
        .filter_map(|n| n.attr("href"))
        .map(|a| a.to_string())
        .collect::<Vec<String>>();

    Ok(product_urls)
}
