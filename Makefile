.PHONY: help

help:
	@grep -E '^[a-zA-Z_-]+:.*?## .*$$' $(MAKEFILE_LIST) | sort | awk 'BEGIN {FS = ":.*?## "}; {printf "\033[36m%-30s\033[0m %s\n", $$1, $$2}'

build: ## Build the project
	docker build -t scraper .

DIR := ${CURDIR}
run: ## Launch the program
	@echo "Launch the container"
	@docker run -it --rm \
	-v $(CURDIR)/data:/app/data \
	--name scraper scraper \
	/bin/ash -c "target/release/asynchronous-concurrency-rust init && \
	target/release/asynchronous-concurrency-rust && \
	cp /app/webscraper.db /app/data/"
	@echo "Changing the owner and the rights of the directory data"
	@sudo chown -R $(id -u ${USER}):$(id -g ${USER}) ./data
	@sudo chmod -R 777 ./data
	@echo "File webscraper.db is in the data directory"