use reqwest::Client;
use select::document::Document;
use select::predicate::*;
use serde_derive::{Deserialize, Serialize};
use std::collections::HashMap;
use std::error::Error;
use std::sync::Arc;
use tokio::runtime;
use tokio::sync::{mpsc, Mutex};

const BASE_URI: &str = "https://www.webscraper.io";

#[derive(Debug, Serialize, Deserialize)]
struct ProductPage {
    category: String,
    sub_category: String,
    url: String,
    body: Box<String>,
}

async fn get_categories(
    client: &Client,
) -> Result<HashMap<String, HashMap<String, String>>, Box<dyn Error>> {
    let res = client
        .get(format!("{}/test-sites/e-commerce/static", self::BASE_URI))
        .send()
        .await?
        .text()
        .await?;

    let document = Document::from(res.as_str());
    let maybe_menu = document.find(Attr("id", "side-menu")).last();
    if maybe_menu.is_none() {
        return Ok(HashMap::new());
    }

    let menu = maybe_menu.unwrap();

    let urls = menu
        .find(Name("a"))
        .filter(|n| n.attr("href").is_some())
        .map(|n| {
            (
                n.text().trim().to_string(),
                n.attr("href").unwrap().to_string(),
            )
        })
        .collect::<HashMap<String, String>>();

    let mut result = HashMap::new();

    for (category, uri) in urls {
        let res = client
            .get(format!("{}{}", self::BASE_URI, uri))
            .send()
            .await?
            .text()
            .await?;

        let document = Document::from(res.as_str());
        let maybe_menu = document.find(Attr("id", "side-menu")).last();
        if maybe_menu.is_none() {
            return Ok(HashMap::new());
        }

        let menu = maybe_menu.unwrap();
        menu.find(And(Name("a"), Class("subcategory-link")))
            .filter(|n| n.attr("href").is_some())
            .map(|n| {
                (
                    n.text().trim().to_string(),
                    n.attr("href").unwrap().to_string(),
                )
            })
            .for_each(|(k, v)| {
                result
                    .entry(category.clone())
                    .or_insert_with(HashMap::new)
                    .insert(k, v);
            });
    }

    return Ok(result);
}

async fn get_product_batches(
    client: Arc<Client>,
    category: String,
    sub_category: String,
    uri: String,
    i: i32,
) -> Result<Vec<ProductPage>, reqwest::Error> {
    let res = client
        .get(format!("{}{}?page={}", self::BASE_URI, uri, i))
        .send()
        .await?
        .text()
        .await?;

    let mut product_pages = Vec::new();
    Document::from(res.as_str())
        .find(
            And(Name("div"), Class("row"))
                .descendant(Class("thumbnail"))
                .descendant(Name("a")),
        )
        .filter_map(|n| n.attr("href"))
        .map(|a| a.to_string())
        .for_each(|url| {
            product_pages.push(ProductPage {
                category: category.clone(),
                sub_category: sub_category.clone(),
                url,
                body: Box::from("".to_string()),
            })
        });

    Ok(product_pages)
}

async fn is_last_page(res: String, current: i32) -> bool {
    if let Some(node) = Document::from(res.as_str())
        .find(And(Name("ul"), Class("pagination")).descendant(Name("a")))
        .filter(|n| n.text().parse::<i32>().is_ok())
        .last()
    {
        if let Ok(page) = node.text().parse::<i32>() {
            return current > page;
        }
    }

    false
}

#[tokio::main]
async fn main() {
    let cpu_pool = runtime::Builder::new_multi_thread()
        .enable_time()
        .enable_io()
        .build()
        .unwrap();

    let client = Arc::new(reqwest::Client::new());
    let categories = get_categories(&client).await.unwrap();

    let (tx, mut rx) = mpsc::channel(100);

    for (category, uris) in categories {
        for (sub_category, uri) in uris {
            'inner: for i in 0.. {
                let tx_clone = tx.clone();
                let client_clone = client.clone();
                let category_clone = category.clone();
                let sub_category_clone = sub_category.clone();
                let uri_clone = uri.clone();

                let finish = cpu_pool.spawn(async move {
                    let res = client_clone
                        .get(format!("{}{}", self::BASE_URI, uri_clone.clone()))
                        .send()
                        .await
                        .unwrap()
                        .text()
                        .await
                        .unwrap();

                    if is_last_page(res, i).await {
                        return false;
                    }

                    let batches = get_product_batches(
                        client_clone,
                        category_clone,
                        sub_category_clone,
                        uri_clone,
                        i,
                    )
                    .await;

                    if let Err(_) = tx_clone.send(batches).await {
                        println!("Receiver dropped");
                        return false;
                    }

                    return true;
                });

                if !finish.await.unwrap() {
                    break 'inner;
                }
            }
        }
    }

    drop(tx);

    tokio::fs::create_dir_all("./data")
        .await
        .expect("Failed to create data dir");

    let result = Arc::new(Mutex::new(Vec::new()));
    while let Some(products_urls) = rx.recv().await {
        if let Ok(product_page_batch) = products_urls {
            for product_page in product_page_batch {
                let page_ref = Arc::new(Mutex::new(product_page));

                let handle = cpu_pool.spawn(async move { download_page(page_ref.clone()).await });

                result.lock().await.push(handle);
            }
        }
    }

    for task in result.lock().await.drain(..) {
        task.await.unwrap();
    }

    cpu_pool.shutdown_background();
}

async fn download_page(page_ref_clone: Arc<Mutex<ProductPage>>) {
    let client = reqwest::Client::new();
    let res = client
        .get(format!(
            "{}{}",
            self::BASE_URI,
            &page_ref_clone.lock().await.url
        ))
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();

    let boxed_res = Box::new(res);
    page_ref_clone.lock().await.body = boxed_res;
    let name = page_ref_clone
        .lock()
        .await
        .url
        .split("/")
        .last()
        .unwrap()
        .to_string();

    let path = format!("./data/{}.json", name);
    let value = serde_json::to_string_pretty(&*page_ref_clone.lock().await).unwrap();

    println!("{}", &path);
    tokio::fs::write(path, value).await.unwrap();
}
