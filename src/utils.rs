use reqwest::{Client, Proxy};
use tokio::fs::File;
use tokio::io::{AsyncSeekExt, AsyncWriteExt};

use std::time::Instant;

pub const PROXY_URL: &str = "http://192.168.192.1:7890"; // consider the real
const CHUNK_SIZE: u64 = 1024 * 1024; // 1MB per chunk
const WAIT_LIMIT: u64 = 10;

async fn test_speed(url: &str, use_proxy: bool) -> Result<f64, reqwest::Error> {
    let client = if use_proxy {
        Client::builder()
            .proxy(Proxy::http(PROXY_URL)?)
            .timeout(std::time::Duration::from_secs(WAIT_LIMIT))
            .build()?
    } else {
        Client::builder()
            .timeout(std::time::Duration::from_secs(WAIT_LIMIT))
            .build()?
    };

    let start = Instant::now();
    let response = client.get(url).send().await?;
    let _bytes = response.bytes().await?;
    let duration = start.elapsed().as_secs_f64();

    Ok(duration)
}

pub async fn choose(url: &str) -> bool {
    println!("Testing speed... wait...");

    let direct_delay = test_speed(url, false).await.unwrap_or_else(|e| {
        println!("Direct: {e}");
        99.0
    });
    let proxy_delay = test_speed(url, true).await.unwrap_or_else(|e| {
        println!("Proxy: {e}");
        99.0
    });

    println!(
        "[Direct: {:.2}s] VS [Proxy: {:.2}s]",
        direct_delay, proxy_delay
    );
    // default to use proxy
    proxy_delay <= direct_delay
}

pub(crate) struct DownloadChunk {
    pub start: u64,
    pub end: u64,
    pub index: usize,
}

pub fn divide_chunks(total_size: u64) -> Vec<DownloadChunk> {
    let mut chunks = Vec::new();
    let mut start = 0;
    let mut index = 0;
    while start < total_size {
        let end = std::cmp::min(start + CHUNK_SIZE - 1, total_size - 1);
        chunks.push(DownloadChunk { start, end, index });
        start = end + 1;
        index += 1;
    }
    println!(
        "Downloading total size ~{}KB, dividing into {} chunks;",
        total_size / 1024,
        chunks.len()
    );
    chunks
}

pub async fn download_chunk(
    client: &Client,
    url: &str,
    chunk: DownloadChunk,
    file: &mut File,
) -> Result<(), String> {
    let response = client
        .get(url)
        .header("Range", format!("bytes={}-{}", chunk.start, chunk.end))
        .send()
        .await
        .map_err(|e| format!("download error of chunk {}: {}", chunk.index, e))?;

    let bytes = response
        .bytes()
        .await
        .map_err(|e| format!("For chunk{}: {}", chunk.index, e))?;

    file.seek(std::io::SeekFrom::Start(chunk.start))
        .await
        .map_err(|e| format!("{}", e))?;
    file.write_all(&bytes).await.map_err(|e| format!("{e}"))?;

    println!("Chunk {} downloaded", chunk.index);
    Ok(())
}
