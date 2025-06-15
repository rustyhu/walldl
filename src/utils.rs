use anyhow::{Context, Result};
use futures_util::StreamExt;
use indicatif::{ProgressBar, ProgressStyle};
use reqwest::{Client, Proxy};
use tokio::time::Instant;
use tokio::{fs::OpenOptions, io::AsyncWriteExt};

const PROXY_URL: &str = "http://192.168.144.1:7890"; // consider the real
const CHUNK_SIZE: u64 = 1024 * 1024; // 1MB per chunk
const WAIT_LIMIT: u64 = 10;

async fn test_speed(client: &Client, url: &str) -> Result<f64> {
    let start = Instant::now();
    let response = client
        .get(url)
        .header("Range", format!("bytes=0-{}", CHUNK_SIZE - 1))
        .send()
        .await
        .context("Failed to send request for speed test")?;

    let bytes = response
        .bytes()
        .await
        .context("Failed to read response bytes")?;
    let duration = start.elapsed().as_secs_f64();
    let speed = ((bytes.len() as u64 / CHUNK_SIZE) as f64) / duration;
    Ok(speed)
}

async fn choose(url: &str) -> Result<bool> {
    println!("Testing speed... wait...");

    use std::time::Duration;
    let client_direct = Client::builder()
        .timeout(Duration::from_secs(WAIT_LIMIT))
        .build()
        .context("Failed to build direct client")?;
    let client_proxy = Client::builder()
        .proxy(Proxy::all(PROXY_URL).context("Invalid proxy URL")?)
        .timeout(Duration::from_secs(WAIT_LIMIT))
        .build()
        .context("Failed to build proxy client")?;

    let direct_speed = test_speed(&client_direct, url).await.unwrap_or_else(|e| {
        println!("Direct test failed: {e}");
        0.0
    });
    let proxy_speed = test_speed(&client_proxy, url).await.unwrap_or_else(|e| {
        println!("Proxy test failed: {e}");
        0.0
    });

    println!(
        "[Direct: {:.2} MB/s] VS [Proxy: {:.2} MB/s]",
        direct_speed, proxy_speed
    );
    Ok(proxy_speed >= direct_speed)
}

pub async fn download_file(url: &str, path: &str) -> Result<()> {
    let use_proxy = choose(url).await?;
    println!(
        "Start via {}...",
        if use_proxy {
            "proxy"
        } else {
            "direct connection"
        }
    );

    let client = if use_proxy {
        Client::builder()
            .proxy(Proxy::http(PROXY_URL).context("Invalid proxy URL")?)
            .build()?
    } else {
        Client::new()
    };
    let response = client.get(url).send().await?;

    let total_size = response
        .content_length()
        .context("Failed to get content length")?;
    let pb = ProgressBar::new(total_size);
    pb.set_style(ProgressStyle::default_bar()
        .template("{spinner:.green} [{elapsed_precise}] [{bar:50.cyan/blue}] {bytes}/{total_bytes} ({eta})")
        .expect("Progress bar template error")
        .progress_chars("#>-"));

    let mut file = OpenOptions::new()
        .create(true)
        .write(true)
        .open(path)
        .await?;

    // stream downloading
    let mut stream = response.bytes_stream();
    let mut downloaded: u64 = 0;
    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        file.write_all(&chunk).await?;
        downloaded += chunk.len() as u64;
        pb.set_position(downloaded);
    }

    pb.finish_with_message("Download completed");
    file.sync_all().await?;

    Ok(())
}
