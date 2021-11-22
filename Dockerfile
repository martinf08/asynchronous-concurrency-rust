FROM rust:1.56-alpine3.14

WORKDIR /app

RUN apk add --no-cache \
    libc-dev \
    libressl-dev \
    sqlite

COPY . .

RUN RUSTFLAGS=-Ctarget-feature=-crt-static cargo build --release

#/app/target/release/asynchronous-concurrency-rust

CMD ["/bin/ash", "-c", "sleep infinity"]


