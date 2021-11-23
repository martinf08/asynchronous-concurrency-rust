# asynchronous-concurrency-rust
![version](https://img.shields.io/badge/cargo-1.56.0-blue)
![version](https://img.shields.io/badge/rustc-1.56.0-blue)
![Latest Stable Version](https://github.com/martinf08/asynchronous-concurrency-rust/workflows/build/badge.svg)

It is an example of using the `async-concurrency` with rust

## Prerequisites
 - Docker 
 - Makefile prerequisites

## Installation
```bash
make build
```
This command will build the docker image.


## Usage
For launching the scraper, run the following command:
```bash
make run
```
- A webscraper.db file is created in the `./data` folder.
- It is a SQLite database. You can explore it to see the products scraped.