.PHONY: help

help:
	@grep -E '^[a-zA-Z_-]+:.*?## .*$$' $(MAKEFILE_LIST) | sort | awk 'BEGIN {FS = ":.*?## "}; {printf "\033[36m%-30s\033[0m %s\n", $$1, $$2}'

build: ## Build the project
	docker build -t scraper .

DIR := ${CURDIR}
run: ## Launch the program
	docker run -it scraper /bin/ash -c "target/release/asynchronous-concurrency-rust init && target/release/asynchronous-concurrency-rust"
