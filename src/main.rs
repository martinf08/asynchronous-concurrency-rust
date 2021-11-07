use std::collections::HashMap;
use std::error::Error;
use std::time::Duration;
use reqwest::{Client, RequestBuilder};
use select::document::Document;
use select::node::Node;
use tokio::macros::support::thread_rng_n;
use tokio::runtime;
use tokio::sync::mpsc;
use tokio::time::*;
use select::predicate::*;
use select::selection::Selection;
use tokio::io::AsyncReadExt;

async fn async_function(name: &str) {
    for i in 0..5 {
        println!("{} : {}", name, i);
        sleep_until(Instant::now() + Duration::from_secs(thread_rng_n(5) as u64)).await;
    }
}

#[tokio::main]
async fn main() {
    let cpu_pool = runtime::Builder::new_multi_thread()
        .enable_time()
        .build()
        .unwrap();
    // let (tx, mut rx) = mpsc::channel(100);

    let client = reqwest::Client::new();
    let categories = get_categories(client).await;


    // for i in 0..10 {
    //     let tx_clone = tx.clone();
    //     cpu_pool.spawn(async move {
    //         for _ in 0..10 {
    //             if let Err(_) = tx_clone.send(i).await {
    //                 println!("Receiver dropped");
    //                 return;
    //             }
    //         }
    //     });
    // }
    //
    // while let Some(i) = rx.recv().await {
    //     cpu_pool.spawn(async move { async_function(&format!("task {}", i)).await });
    // }
}

async fn get_categories(client: Client) -> Result<Vec<String>, Box<dyn Error>> {
    let res = client.get("https://www.webscraper.io/test-sites/e-commerce/static").send().await?.text().await?;

    let node = Document::from(res.as_str())
        .find(Attr("id", "side-menu").descendant(Name("a")))
        .into_selection()
        .iter()
        // .inspect(|x| { dbg!(&x); })
        .filter_map()
        .filter(|n| n.attr("href").is_some())
        .map(|n| (n.text().trim().to_string(), n.attr("href").unwrap().to_string()))
        .collect::<HashMap<String, String>>();

    // dbg!(&node);

    Ok(Vec::new())
}