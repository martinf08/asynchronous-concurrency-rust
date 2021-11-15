use futures::*;
use reqwest::Client;
use select::document::Document;
use select::predicate::*;
use serde_derive::{Deserialize, Serialize};
use sqlx::sqlite::{SqlitePool, SqlitePoolOptions};
use std::collections::HashMap;
use std::error::Error;
use std::sync::Arc;
use std::thread::JoinHandle;
use structopt::StructOpt;
use tokio::fs::File;
use tokio::runtime;
use tokio::sync::Mutex;
use tokio::task::JoinHandle as OtherJoinHandle;

const BASE_URI: &str = "https://www.webscraper.io";
const DB_NAME: &str = "webscraper.db";
const WORKERS: usize = 2;

#[derive(StructOpt, Debug)]
struct Args {
    #[structopt(default_value = "scrape")]
    cmd: String,
}

pub async fn get_database_pool() -> Result<SqlitePool, anyhow::Error> {
    let db_pool = SqlitePoolOptions::new()
        .max_connections(WORKERS as u32)
        .connect(&*format!("sqlite://{}", DB_NAME))
        .await?;

    Ok(db_pool)
}

async fn init() -> anyhow::Result<()> {
    let filename = format!("./{}", DB_NAME);
    match tokio::fs::remove_file(&filename).await {
        _ => (),
    };

    File::create(&filename).await?;

    let db_pool = get_database_pool().await?;

    sqlx::query(
        r#"
CREATE TABLE IF NOT EXISTS pages (
  id INTEGER PRIMARY KEY NOT NULL,
  url TEXT NOT NULL,
  html TEXT NOT NULL,
  parsed INTEGER NOT NULL,
  raw_data TEXT NOT NULL
);
        "#,
    )
    .execute(&db_pool)
    .await?;

    db_pool.close().await;
    Ok(())
}

#[derive(Debug, Serialize, Deserialize, Clone)]
struct ProductPage {
    category: String,
    sub_category: String,
    url: String,
    html: Box<String>,
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
    client: Client,
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
                url: format!("{}{}", self::BASE_URI, url),
                html: Box::from("".to_string()),
            })
        });

    dbg!(&product_pages.len());
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

async fn get_product_page_list() -> anyhow::Result<Arc<Mutex<Vec<ProductPage>>>> {
    let cpu_pool = runtime::Builder::new_multi_thread()
        .worker_threads(WORKERS)
        .enable_time()
        .enable_io()
        .build()?;

    let client = Arc::new(reqwest::Client::new());
    let categories = get_categories(&client).await.unwrap();

    let handlers = Arc::new(Mutex::new(Vec::new()));
    let finish = Arc::new(Mutex::new(false));
    let i = 0;
    for (category, uris) in categories {
        let finish_clone = Arc::clone(&finish);
        for (sub_category, uri) in uris {
            *finish_clone.lock().await = false;
            let category_clone = category.clone();
            let sub_category_clone = sub_category.clone();
            let uri_clone = uri.clone();
            let finish_clone_clone = Arc::clone(&finish_clone);

            let handler = cpu_pool.spawn(async move {
                let client = Client::new();
                let res = client
                    .get(format!("{}{}", self::BASE_URI, uri_clone.clone()))
                    .send()
                    .await
                    .unwrap()
                    .text()
                    .await
                    .unwrap();

                if is_last_page(res, i).await {
                    *finish_clone_clone.lock().await = true;
                    return None;
                }

                if let Ok(batches) = get_product_batches(
                    client.clone(),
                    category_clone,
                    sub_category_clone,
                    uri_clone,
                    i,
                )
                .await
                {
                    return Some(batches);
                }

                None
            });

            handlers.lock().await.push(handler);
        }
    }

    let result = Arc::new(Mutex::new(Vec::new()));

    for product_pages in handlers.lock().await.drain(..) {
        let result_clone = Arc::clone(&result);
        if let Some(mut page_result) = product_pages.await? {
            let result_clone_clone = Arc::clone(&result_clone);
            for page in page_result.drain(..) {
                let result_clone_clone_clone = Arc::clone(&result_clone_clone);
                cpu_pool.spawn(async move {
                    let mut lock = result_clone_clone_clone.lock().await;
                    lock.push(page);
                });
            }
        }
    }

    cpu_pool.shutdown_background();
    Ok(result)
}

async fn add_product_pages(
    product_page_list: Arc<Mutex<Vec<ProductPage>>>,
) -> anyhow::Result<Arc<Mutex<Vec<ProductPage>>>> {
    let cpu_pool = runtime::Builder::new_multi_thread()
        .worker_threads(WORKERS)
        .enable_time()
        .enable_io()
        .build()?;

    let mut handlers = Vec::new();

    for product_page in product_page_list.lock().await.iter() {
        let mutex_page = Arc::new(Mutex::new(product_page.clone()));

        let handler: OtherJoinHandle<(String, Box<String>)> = cpu_pool.spawn({
            let mutex_page_clone = Arc::clone(&mutex_page);

            async move {
                let lock = mutex_page_clone.lock().await;
                let client = reqwest::Client::new();
                let res = client
                    .get(&lock.url)
                    .send()
                    .await
                    .unwrap()
                    .text()
                    .await
                    .unwrap();

                (String::from(&lock.url), Box::from(res))
            }
        });

        handlers.push(handler);
    }

    for handler in handlers {
        let (url, html) = handler.await?;
        for product_page in product_page_list.lock().await.iter_mut() {
            if product_page.url == url {
                product_page.html = html.clone();
            }
        }
    }

    cpu_pool.shutdown_background();

    Ok(product_page_list)
}

async fn insert_product_pages(product_pages: Arc<Mutex<Vec<ProductPage>>>) -> anyhow::Result<()> {
    let pages = &*product_pages.lock().await.clone();

    let stream = stream::iter(pages.to_vec().into_iter());
    let db_pool = Arc::new(get_database_pool().await?);

    Ok(stream
        .for_each_concurrent(None, |page| {
            let db_pool_clone = Arc::clone(&db_pool);
            async move {
                sqlx::query(
                    "INSERT INTO pages (url, html, parsed, raw_data) VALUES (?1, ?2, ?3, ?4)",
                )
                .bind(&page.url)
                .bind(&*page.html)
                .bind(0)
                .bind("".to_string())
                .execute(&*db_pool_clone)
                .await
                .unwrap();
            }
        })
        .await)
}

#[paw::main]
#[tokio::main]
async fn main(args: Args) -> anyhow::Result<()> {
    match args.cmd {
        x if x == "init" => init().await?,
        x if x == "scrape" => scrape().await?,
        _ => {
            println!("{}", "Invalid command");
            return Ok(());
        }
    }

    Ok(())
}

async fn scrape() -> anyhow::Result<()> {
    let product_page_list = get_product_page_list().await?;
    let product_pages = add_product_pages(product_page_list).await?;
    insert_product_pages(product_pages).await?;

    Ok(())
}
