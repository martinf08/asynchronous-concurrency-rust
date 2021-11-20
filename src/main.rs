use futures::*;
use reqwest::Client;
use select::document::Document;
use select::predicate::*;
use serde_derive::{Deserialize, Serialize};
use sqlx::sqlite::{SqlitePool, SqlitePoolOptions};
use std::cell::RefCell;
use std::collections::HashMap;
use std::error::Error;
use std::rc::Rc;
use std::sync::Arc;
use structopt::StructOpt;
use tokio::fs::File;
use tokio::sync::Mutex;

const BASE_URI: &str = "https://www.webscraper.io";
const DB_NAME: &str = "webscraper.db";
const WORKERS: usize = 4;

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
    uri: String,
    product_urls: Vec<String>,
    last_page: Option<u16>,
    index: u16,
    iter_index: u16,
}

impl ProductPage {
    fn new(category: Rc<String>, sub_category: String, uri: String) -> Self {
        Self {
            category: category.to_string(),
            sub_category,
            uri,
            product_urls: Vec::new(),
            last_page: None,
            index: 1,
            iter_index: 1,
        }
    }

    fn get_index_url(&self) -> String {
        format!("{}{}?page={}", self::BASE_URI, self.uri, self.index)
    }

    fn get_last_page(&self, res: &String) -> Option<u16> {
        if let Some(node) = Document::from(res.as_str())
            .find(And(Name("ul"), Class("pagination")).descendant(Name("a")))
            .filter(|n| n.text().parse::<i32>().is_ok())
            .last()
        {
            if let Ok(page) = node.text().parse::<i32>() {
                return Some(page as u16);
            }
        }

        None
    }

    fn get_product_urls(&self, res: &String) -> Vec<String> {
        Document::from(res.as_str())
            .find(
                And(Name("div"), Class("row"))
                    .descendant(Class("thumbnail"))
                    .descendant(Name("a")),
            )
            .filter_map(|n| n.attr("href"))
            .map(|a| a.to_string())
            .collect::<Vec<String>>()
    }

    async fn parse_next_page(&mut self) -> anyhow::Result<bool> {
        if self.index > self.last_page.unwrap_or(1) {
            return Ok(false);
        }

        let client = Client::new();
        let res = client
            .get(&self.get_index_url())
            .send()
            .await?
            .text()
            .await?;

        self.last_page = self.get_last_page(&res);
        self.product_urls.extend(self.get_product_urls(&res));
        self.index += 1;
        self.iter_index += 1;

        Ok(true)
    }
}

impl Iterator for ProductPage {
    type Item = u16;

    fn next(&mut self) -> Option<Self::Item> {
        self.iter_index += 1;
        if self.last_page.is_none() || self.iter_index > self.last_page.unwrap() {
            self.iter_index = self.index;
            return None;
        }
        Some(self.iter_index)
    }
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

async fn get_product_page_list() -> anyhow::Result<Vec<ProductPage>> {
    let client = Arc::new(reqwest::Client::new());
    let categories = get_categories(&client).await.unwrap();

    let pages = Rc::new(RefCell::new(Vec::new()));
    stream::iter(categories.into_iter())
        .for_each(|(category, uris)| {
            let ref_page = Rc::clone(&pages);

            async move {
                let ref_page_clone = Rc::clone(&ref_page);
                let ref_category = Rc::new(category);

                stream::iter(uris.into_iter())
                    .for_each(|(sub_category, uri)| {
                        let ref_page_clone_clone = Rc::clone(&ref_page_clone);
                        let ref_category_clone = Rc::clone(&ref_category);

                        async move {
                            let mut product_page = ProductPage::new(
                                ref_category_clone,
                                sub_category.clone(),
                                uri.clone(),
                            );

                            product_page
                                .parse_next_page()
                                .await
                                .expect("Failed to parse product page");

                            ref_page_clone_clone.borrow_mut().push(product_page);
                        }
                    })
                    .await;
            }
        })
        .await;

    let mut product_pages = pages.take();
    for page in product_pages.iter_mut() {
        let atomic_page = Arc::new(Mutex::new(page));
        let indexes = atomic_page.lock().await.into_iter().collect::<Vec<_>>();

        stream::iter(indexes.into_iter())
            .for_each_concurrent(WORKERS, |_| {
                let atomic_page_clone = Arc::clone(&atomic_page);
                async move {
                    atomic_page_clone
                        .lock()
                        .await
                        .parse_next_page()
                        .await
                        .expect("Failed to parse product page");
                }
            })
            .await;
    }

    Ok(product_pages)
}

async fn insert_product_pages(product_pages: Arc<Mutex<Vec<ProductPage>>>) -> anyhow::Result<()> {
    todo!();
    // let pages = &*product_pages.lock().await.clone();
    //
    // let stream = stream::iter(pages.to_vec().into_iter());
    // let db_pool = Arc::new(get_database_pool().await?);
    //
    // Ok(stream
    //     .for_each_concurrent(WORKERS, |page| {
    //         let db_pool_clone = Arc::clone(&db_pool);
    //         async move {
    //             sqlx::query(
    //                 "INSERT INTO pages (url, html, parsed, raw_data) VALUES (?1, ?2, ?3, ?4)",
    //             )
    //             .bind(&page.url)
    //             .bind(&*page.html)
    //             .bind(0)
    //             .bind("".to_string())
    //             .execute(&*db_pool_clone)
    //             .await
    //             .unwrap();
    //         }
    //     })
    //     .await)
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
    dbg!(product_page_list);
    // let product_pages = add_product_pages(product_page_list).await?;
    // insert_product_pages(product_pages).await?;

    Ok(())
}
