// use futures::future::join_all;
use reqwest::{Client, Proxy};
use std::error::Error;
use tokio::fs::OpenOptions;
use tokio::sync::Semaphore;

mod utils;
use utils::{choose, divide_chunks, download_chunk, PROXY_URL};

async fn download_file(url: &str, path: &str) -> Result<(), Box<dyn Error>> {
    let use_proxy = choose(url).await;
    println!(
        "Start via {}...",
        if use_proxy {
            "proxy"
        } else {
            "direct connection"
        }
    );

    let client = if use_proxy {
        Client::builder().proxy(Proxy::http(PROXY_URL)?).build()?
    } else {
        Client::new()
    };
    let response = client.get(url).send().await?;
    let total_size = response.content_length().unwrap_or(0);

    // divide according by size
    let chunks = divide_chunks(total_size);

    let file_init = OpenOptions::new()
        .create(true)
        .write(true)
        .open(path)
        .await?;
    file_init.set_len(total_size).await?;

    // Semaphore to limit the number of curcurrent tasks running
    let semaphore = std::sync::Arc::new(Semaphore::new(2));
    let mut tasks = Vec::new();
    for chunk in chunks {
        let client = client.clone();
        let url = url.to_string();
        let mut file = OpenOptions::new().write(true).open(path).await?;
        let semaphore = semaphore.clone();

        tasks.push(tokio::spawn(async move {
            let _permit = semaphore.acquire().await.unwrap();
            download_chunk(&client, &url, chunk, &mut file).await
        }));
    }

    // join_all(tasks).await;
    for task in tasks {
        let chunk_res = task.await.unwrap_or_else(|e| {
            println!("{e}");
            Ok(())
        });
        if let Err(s) = chunk_res {
            println!("{s}");
        }
    }

    Ok(())
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    //! consider single url to download currently;

    let mut args = std::env::args();
    let url = args.nth(1).ok_or("Missing URL argument!")?;

    // read path from cmd args - optional
    let path = match args.nth(0) {
        Some(op) if op == "-o" => args.nth(0).unwrap_or_else(|| {
            eprintln!("Missing path argument!");
            std::process::exit(1);
        }),
        // default: cur path + filename from url tail
        _ => "./".to_owned() + url.split('/').last().unwrap_or("outfile"),
    };
    // ("https://example.com/file2.txt", "file2.txt"),

    match download_file(&url, &path).await {
        Ok(_) => println!("[main]Done!"),
        Err(e) => eprintln!("[main]Failed to download {url}: {e}"),
    }

    Ok(())
}
