use anyhow::{Context, Result};
use futures_util::StreamExt;
use indicatif::{ProgressBar, ProgressStyle};
use reqwest::{Client, Proxy};
use tokio::time::Instant;
use tokio::{fs::OpenOptions, io::AsyncWriteExt};

const PROXY_URL: &str = "http://192.168.144.1:7890"; // consider the real
const CHUNK_SIZE: u64 = 1024 * 1024; // 1MB per chunk - used for actual download chunking if any, not speed test
const SPEED_TEST_CHUNK_SIZE: u64 = 200 * 1024; // 200KB for speed test
const WAIT_LIMIT: u64 = 10;

// Helper function for speed calculation
fn calculate_speed_mbps(bytes_len_f64: f64, duration_secs_f64: f64) -> f64 {
    if duration_secs_f64 == 0.0 {
        return 0.0; // Avoid division by zero; effectively 0 speed if no time elapsed or no data.
    }
    // Speed in MB/s
    (bytes_len_f64 / 1024.0 / 1024.0) / duration_secs_f64
}

async fn test_speed(client: &Client, url: &str) -> Result<f64> {
    let start = Instant::now();
    let response = client
        .get(url)
        .header("Range", format!("bytes=0-{}", SPEED_TEST_CHUNK_SIZE - 1)) // Use SPEED_TEST_CHUNK_SIZE
        .send()
        .await
        .context("Failed to send request for speed test")?;

    let bytes = response
        .bytes()
        .await
        .context("Failed to read response bytes")?;
    let duration_secs = start.elapsed().as_secs_f64();

    Ok(calculate_speed_mbps(bytes.len() as f64, duration_secs))
}

async fn choose(url: &str) -> Result<bool> { // Original signature
    println!("Testing speed... wait...");

    use std::time::Duration;
    let client_direct = Client::builder()
        .timeout(Duration::from_secs(WAIT_LIMIT))
        .build()
        .context("Failed to build direct client")?;
    let client_proxy = Client::builder()
        .proxy(Proxy::all(PROXY_URL).context("Invalid proxy URL")?) // Use hardcoded PROXY_URL
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

    // If both tests failed or yielded 0.0 speed, return an error.
    if direct_speed <= 0.0 && proxy_speed <= 0.0 {
        return Err(anyhow::anyhow!("Both direct and proxy speed tests failed or yielded no speed."));
    }

    Ok(proxy_speed > direct_speed) // Use proxy only if strictly faster
}

pub async fn download_file(url: &str, path: &str) -> Result<()> { // Original signature
    let use_proxy = match choose(url).await {
        Ok(should_use_proxy) => {
            println!(
                "[download_file]Choose decided: Will use {} connection.",
                if should_use_proxy { "proxy" } else { "direct" }
            );
            should_use_proxy
        }
        Err(e) => {
            eprintln!("[download_file]Speed test error: {}. Defaulting to proxy download.", e);
            true // Default to using proxy if choose fails
        }
    };

    println!( // This message now confirms the actual path taken
        "Attempting download via {}...",
        if use_proxy {
            "proxy"
        } else {
            "direct connection"
        }
    );

    let client = if use_proxy {
        Client::builder()
            .proxy(Proxy::all(PROXY_URL).context("Invalid proxy URL for download")?) // Use Proxy::all
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

#[cfg(test)]
mod tests {
    use super::*; // To import calculate_speed_mbps

    const DELTA: f64 = 1e-6; // A small tolerance for floating-point comparisons

    #[test]
    fn test_speed_calculations() {
        // 1MB in 1s = 1.0 MB/s
        assert!((calculate_speed_mbps(1024.0 * 1024.0, 1.0) - 1.0).abs() < DELTA);

        // 0 bytes in 1s = 0.0 MB/s
        assert!((calculate_speed_mbps(0.0, 1.0) - 0.0).abs() < DELTA);

        // 2MB in 1s = 2.0 MB/s
        assert!((calculate_speed_mbps(1024.0 * 1024.0 * 2.0, 1.0) - 2.0).abs() < DELTA);

        // 1MB in 0.5s = 2.0 MB/s
        assert!((calculate_speed_mbps(1024.0 * 1024.0, 0.5) - 2.0).abs() < DELTA);

        // 1MB in 0s = 0.0 MB/s (handles division by zero)
        assert!((calculate_speed_mbps(1024.0 * 1024.0, 0.0) - 0.0).abs() < DELTA);

        // 0.5MB in 1s = 0.5 MB/s
        assert!((calculate_speed_mbps(0.5 * 1024.0 * 1024.0, 1.0) - 0.5).abs() < DELTA);

        // Another test: 2.5MB in 2.0s = 1.25 MB/s
        assert!((calculate_speed_mbps(2.5 * 1024.0 * 1024.0, 2.0) - 1.25).abs() < DELTA);
    }
}
