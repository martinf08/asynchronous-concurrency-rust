use futures::*;
use indicatif::{MultiProgress, ProgressBar, ProgressStyle};
use regex::Regex;
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
use tokio::time::Instant;

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
    println!("Remove old webscraper.db sqlite database file");
    let filename = format!("./{}", DB_NAME);
    match tokio::fs::remove_file(&filename).await {
        _ => (),
    };

    File::create(&filename).await?;

    let db_pool = get_database_pool().await?;

    sqlx::query(
        r#"
CREATE TABLE IF NOT EXISTS products (
  id INTEGER PRIMARY KEY NOT NULL,
  uid VARCHAR(255) NOT NULL,
  category VARCHAR(255) NOT NULL,
  sub_category VARCHAR(255) NOT NULL,
  name VARCHAR(255) NOT NULL,
  origin VARCHAR(255) NOT NULL,
  cost INTEGER NOT NULL,
  description TEXT,
  color VARCHAR(255),
  size VARCHAR(255),
  review_count INTEGER,
  review_stars INTEGER
);
        "#,
    )
    .execute(&db_pool)
    .await?;

    db_pool.close().await;

    println!("Database was successfully initialized");
    Ok(())
}

#[derive(Debug, Serialize, Deserialize, Clone)]
struct ProductPage {
    category: String,
    sub_category: String,
    uri: String,
    products_uri: Vec<String>,
    last_page: Option<u16>,
    index: u16,
    iter_index: u16,
    process: u8,
}

impl ProductPage {
    fn new(category: Rc<String>, sub_category: String, uri: String) -> Self {
        Self {
            category: category.to_string(),
            sub_category,
            uri,
            products_uri: Vec::new(),
            last_page: None,
            index: 1,
            iter_index: 1,
            process: 1,
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
        let client = Client::new();
        let res = client
            .get(&self.get_index_url())
            .send()
            .await?
            .text()
            .await?;

        self.last_page = self.get_last_page(&res);
        self.products_uri.extend(self.get_product_urls(&res));
        self.index += 1;
        self.iter_index += 1;

        if self.last_page.is_some() && (self.index > self.last_page.unwrap()) {
            self.process = 0
        }

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

#[derive(Debug, Clone)]
struct Product {
    uid: String,
    category: String,
    sub_category: String,
    name: String,
    origin: String,
    cost: f32,
    description: String,
    colors: Vec<String>,
    sizes: Vec<String>,
    review_count: u32,
    review_stars: u32,
}

impl Default for Product {
    fn default() -> Self {
        Product {
            uid: String::new(),
            category: String::new(),
            sub_category: String::new(),
            name: String::new(),
            origin: String::new(),
            cost: 0.0,
            description: String::new(),
            colors: Vec::new(),
            sizes: Vec::new(),
            review_count: 0,
            review_stars: 0,
        }
    }
}

impl Product {
    fn parse_html(&mut self, res: &String) -> anyhow::Result<()> {
        let doc = Document::from(res.as_str());

        let caption = Document::from(
            doc.find(And(Name("div"), Class("caption")))
                .next()
                .ok_or(anyhow::anyhow!("Div caption not found"))?
                .html()
                .as_str(),
        );

        self.name = caption
            .find(And(Name("h4"), Not(Attr("class", ()))))
            .next()
            .map(|n| n.text())
            .unwrap_or_default();

        self.cost = caption
            .find(And(Name("h4"), Class("price")))
            .next()
            .map(|n| n.text())
            .unwrap_or_default()
            .replace("$", "")
            .parse::<f32>()
            .unwrap_or_default();

        self.description = caption
            .find(And(Name("p"), Class("description")))
            .next()
            .map(|n| n.text())
            .unwrap_or_default();

        let frame = Document::from(
            doc.find(And(Name("div"), Class("row")).descendant(Class("thumbnail")))
                .next()
                .ok_or(anyhow::anyhow!("Div thumbnail not found"))?
                .html()
                .as_str(),
        );

        self.colors = frame
            .find(And(Name("select"), Attr("aria-label", "color")).descendant(Name("option")))
            .filter_map(|n| n.attr("value"))
            .map(|v| v.to_string())
            .filter(|v| !v.is_empty())
            .collect::<Vec<String>>();

        self.sizes = frame
            .find(
                And(Name("div"), Class("swatches"))
                    .descendant(And(Name("button"), Not(Attr("disabled", ())))),
            )
            .filter_map(|n| n.attr("value"))
            .map(|v| v.to_string())
            .filter(|v| !v.is_empty())
            .collect::<Vec<String>>();

        let review_count_raw = frame
            .find(And(Name("div"), Class("ratings")).descendant(Name("p")))
            .map(|n| n.text())
            .collect::<String>();

        let re = Regex::new(r"\d+")?;

        if let Some(caps) = re.captures(&review_count_raw) {
            self.review_count = caps[0].parse::<u32>().unwrap_or_default();
        }

        self.review_stars = frame
            .find(
                And(Name("div"), Class("ratings"))
                    .descendant(And(Name("span"), Class("glyphicon-star"))),
            )
            .count() as u32;

        Ok(())
    }

    fn generate_variants(product: Self) -> Vec<Self> {
        let mut variants = Vec::new();

        match (product.colors.len(), product.sizes.len()) {
            (0, 0) => {
                panic!("No colors or sizes found for product : {}", &product.origin);
            }
            (0, _) => {
                for size in product.sizes.iter() {
                    let mut variant = product.clone();
                    variant.sizes = vec![size.clone()];
                    variants.push(variant);
                }
            }
            (_, 0) => {
                for color in product.colors.iter() {
                    let mut variant = product.clone();
                    variant.colors = vec![color.clone()];
                    variants.push(variant);
                }
            }
            (_, _) => {
                for color in product.colors.iter() {
                    for size in product.sizes.iter() {
                        let mut variant = product.clone();
                        variant.colors = vec![color.clone()];
                        variant.sizes = vec![size.clone()];
                        variants.push(variant);
                    }
                }
            }
        }

        variants
    }

    fn build_uid(&mut self) {
        let source_id = self.origin.split("/").last().unwrap_or_default();

        match (self.colors.len(), self.sizes.len()) {
            (0, 0) => {
                panic!("No colors or sizes found for product : {}", &self.origin);
            }
            (0, _) => {
                self.uid = format!("{}-{}", source_id, self.sizes[0]);
            }
            (_, 0) => {
                self.uid = format!("{}-{}", source_id, self.colors[0]);
            }
            (_, _) => {
                self.uid = format!("{}-{}-{}", source_id, self.colors[0], self.sizes[0]);
            }
        }
    }

    async fn insert(self, pool: Arc<SqlitePool>) {
        sqlx::query(
            r#"
INSERT INTO products (uid, category, sub_category, name, origin, cost, description, color, size, review_count, review_stars)
VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
"#,
    )
            .bind(self.uid)
            .bind(self.category)
            .bind(self.sub_category)
            .bind(self.name)
            .bind(self.origin)
            .bind(self.cost)
            .bind(self.description)
            .bind(self.colors.join(" "))
            .bind(self.sizes.join(" "))
            .bind(self.review_count)
            .bind(self.review_stars)
            .execute(&*pool)
            .await
            .unwrap();
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

    let multi_progress_bar = Arc::new(MultiProgress::new());
    let style =
        ProgressStyle::default_bar().template("{prefix} {bar:40.green/yellow} {pos:>7}/{len:7}");

    println!("Parsing category pages");

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
        let mut indexes = atomic_page.lock().await.into_iter().collect::<Vec<_>>();
        indexes.push(0);

        let progress_bar = multi_progress_bar.add(ProgressBar::new(indexes.len() as u64));
        progress_bar.set_style(style.clone());
        progress_bar.tick();
        progress_bar.set_prefix(format!("{}", &atomic_page.lock().await.sub_category));

        stream::iter(indexes.into_iter())
            .for_each_concurrent(WORKERS, |i| {
                progress_bar.inc(1);
                progress_bar.set_message(format!("item #{}", i + 1));

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

        progress_bar.finish();
    }

    Ok(product_pages)
}

async fn parse_pages(product_pages: Vec<ProductPage>) -> anyhow::Result<()> {
    let db_pool = Arc::new(get_database_pool().await?);

    let nb_uri = product_pages
        .iter()
        .map(|p| p.products_uri.len())
        .sum::<usize>();
    let progress_bar = Arc::new(ProgressBar::new(nb_uri as u64));
    let style =
        ProgressStyle::default_bar().template("{prefix} {bar:40.green/yellow} {pos:>7}/{len:7}");

    progress_bar.set_style(style);
    progress_bar.set_prefix("Parsing products");
    progress_bar.tick();

    for product_page in product_pages.iter() {
        let progress_bar_clone = Arc::clone(&progress_bar);

        stream::iter(&product_page.products_uri)
            .for_each_concurrent(WORKERS, |uri| {
                let progress_bar_clone_clone = Arc::clone(&progress_bar_clone);
                let db_pool_clone = Arc::clone(&db_pool);
                async move {
                    let product =
                        get_product(uri, &product_page.category, &product_page.sub_category)
                            .await
                            .expect("Failed to get product");

                    progress_bar_clone_clone.inc(1);
                    let variants = Product::generate_variants(product);

                    stream::iter(variants.into_iter())
                        .for_each_concurrent(WORKERS, |mut variant| {
                            let db_pool_clone_clone = Arc::clone(&db_pool_clone);
                            async move {
                                variant.build_uid();
                                variant.insert(db_pool_clone_clone).await;
                            }
                        })
                        .await;
                }
            })
            .await;
    }

    Ok(())
}

async fn get_product(
    uri: &String,
    category: &String,
    sub_category: &String,
) -> anyhow::Result<Product> {
    let client = Client::new();

    let res = client
        .get(format!("{}{}", self::BASE_URI, uri))
        .send()
        .await?
        .text()
        .await?;

    let mut product = Product::default();

    product.category = category.to_string();
    product.sub_category = sub_category.to_string();
    product.origin = uri.to_string();

    product.parse_html(&res)?;

    Ok(product)
}

#[paw::main]
#[tokio::main]
async fn main(args: Args) -> anyhow::Result<()> {
    let now = Instant::now();

    match args.cmd {
        x if x == "init" => init().await?,
        x if x == "scrape" => scrape().await?,
        _ => {
            println!("{}", "Invalid command");
            return Ok(());
        }
    }

    println!("Finish in {:.2} secs", now.elapsed().as_secs_f64());
    Ok(())
}

async fn scrape() -> anyhow::Result<()> {
    let product_page_list = get_product_page_list().await?;
    parse_pages(product_page_list).await?;
    println!("Product saved in webscraper.db sqlite file");

    Ok(())
}
