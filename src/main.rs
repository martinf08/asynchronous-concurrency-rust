use std::time::Duration;
use tokio::macros::support::thread_rng_n;
use tokio::runtime;
use tokio::time::*;
use tokio::sync::mpsc;

async fn async_function(name: &str) {
    for i in 0..5 {
        println!("{} : {}", name, i);
        sleep_until(Instant::now() + Duration::from_secs(thread_rng_n(5) as u64)).await;
    }
}

#[tokio::main]
async fn main() {
    let cpu_pool = runtime::Builder::new_multi_thread().enable_time().build().unwrap();
    let (tx, mut rx) = mpsc::channel(100);

    for i in 0..10 {
        let tx_clone = tx.clone();
        cpu_pool.spawn( async move {
            for _ in 0..10 {
                if let Err(_) = tx_clone.send(i).await {
                    println!("Receiver dropped");
                    return;
                }
            }
        });
    }

    while let Some(i) = rx.recv().await {
        cpu_pool.spawn(async move {
            async_function(&format!("task {}", i)).await
        });
    }
}
