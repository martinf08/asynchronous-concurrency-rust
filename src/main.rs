use reqwest::Client;
use select::document::Document;
use select::predicate::*;
use serde_derive::{Deserialize, Serialize};
use std::collections::HashMap;
use std::env::var;
use std::error::Error;
use std::sync::Arc;
use tokio::runtime;
use tokio::sync::{mpsc, Mutex};
use structopt::StructOpt;
use sqlx::sqlite::SqlitePool;
use tokio::fs::File;

const BASE_URI: &str = "https://www.webscraper.io";
const DB_NAME: &str = "webscraper.db";

#[derive(StructOpt, Debug)]
struct Args {
    #[structopt(default_value = "scrape")]
    cmd: String,
}

pub async fn get_database_pool() -> Result<SqlitePool, anyhow::Error> {
    let path = format!("./{}", DB_NAME);
    File::create(&path).await?;
    let db_pool = SqlitePool::connect(&*format!("sqlite://{}", DB_NAME)).await?;

    Ok(db_pool)
}

async fn init(db_pool: &Arc<SqlitePool>) -> anyhow::Result<()> {
    let filename = format!("./{}", DB_NAME);
    match tokio::fs::remove_file(&filename).await {
       _ => ()
    };

    tokio::fs::File::create(&filename).await?;

    sqlx::query(
        r#"
CREATE TABLE IF NOT EXISTS pages (
  id INTEGER PRIMARY KEY NOT NULL,
  product_id TEXT NOT NULL,
  raw_data TEXT NOT NULL
);
        "#
    ).execute(&**db_pool)
        .await?;

    db_pool.close().await;
    Ok(())
}

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

async fn insert_page(db_pool: &Arc<SqlitePool>, page_ref_clone: Arc<Mutex<ProductPage>>) -> anyhow::Result<()> {
    let client = reqwest::Client::new();
    let res = client
        .get(format!(
            "{}{}",
            self::BASE_URI,
            &page_ref_clone.lock().await.url
        ))
        .send()
        .await?
        .text()
        .await?;

    let boxed_res = Box::new(res);
    page_ref_clone.lock().await.body = boxed_res;
    let name = page_ref_clone
        .lock()
        .await
        .url
        .split("/")
        .last()
        .ok_or(anyhow::anyhow!("url is empty"))?
        .to_string();

    dbg!("ok");
    sqlx::query("INSERT INTO pages (product_id, body) VALUES (?1, ?2)")
        .bind(&page_ref_clone.lock().await.url)
        .bind(&page_ref_clone.lock().await.category)
        .execute(&**db_pool)
        .await?;

    let path = format!("./data/{}.json", name);
    let value = serde_json::to_string_pretty(&*page_ref_clone.lock().await)?;

    println!("{}", &path);
    tokio::fs::write(path, value).await?;

    Ok(())
}

async fn scrape(cpu_pool: &runtime::Runtime, db_pool: &Arc<SqlitePool>) -> anyhow::Result<()> {

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

    // let result = Arc::new(Mutex::new(Vec::new()));

    while let Some(products_urls) = rx.recv().await {
        let db_pool_clone = db_pool.clone();
        if let Ok(product_page_batch) = products_urls {
            for product_page in product_page_batch {
                let db_pool_clone_clone = db_pool_clone.clone();
                let page_ref = Arc::new(Mutex::new(product_page));

                let handle = cpu_pool.spawn(async move { insert_page(&db_pool_clone_clone, page_ref.clone()).await });

                result.lock().await.push(handle);
            }
        }
    }

    for task in result.lock().await.drain(..) {
        task.await??;
    }


    Ok(())
}


#[paw::main]
#[tokio::main]
async fn main(args: Args) -> anyhow::Result<()> {
    let cpu_pool = runtime::Builder::new_multi_thread()
        .enable_time()
        .enable_io()
        .build()?;

    let db_pool = Arc::new(get_database_pool().await?);

    match args.cmd {
        x if x == "init" => init(&db_pool).await?,
        x if x == "scrape" => scrape(&cpu_pool, &db_pool).await?,
        _ => {
            println!("{}", "Invalid command");
            return Ok(());
        }
    }

    cpu_pool.shutdown_background();

    Ok(())
}
