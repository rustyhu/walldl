use std::{io::ErrorKind, path::Path, time::Duration};

use anyhow::{anyhow, bail, ensure, Context, Result};
use indicatif::{ProgressBar, ProgressStyle};
use reqwest::{header, Client, Proxy, Response, StatusCode, Url};
use tempfile::NamedTempFile;
use tokio::{
    fs,
    io::{AsyncWriteExt, BufWriter},
    time::{timeout, timeout_at, Instant},
};

const PROBE_BYTES: u64 = 512 * 1024;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const PROBE_TIMEOUT: Duration = Duration::from_secs(10);
const IDLE_TIMEOUT: Duration = Duration::from_secs(30);

async fn get_proxy_url() -> Result<Url> {
    let file = match fs::read_to_string("proxy.txt").await {
        Ok(value) => value,
        Err(error) if error.kind() == ErrorKind::NotFound => String::new(),
        Err(error) => return Err(error).context("Failed to read proxy.txt"),
    };
    let value = if !file.trim().is_empty() {
        file
    } else {
        match std::env::var("PROXY") {
            Ok(value) if !value.trim().is_empty() => value,
            Ok(_) | Err(std::env::VarError::NotPresent) => "http://127.0.0.1:8080".into(),
            Err(_) => bail!("PROXY must be valid UTF-8"),
        }
    };
    let url = Url::parse(value.trim()).context("Invalid proxy URL")?;
    ensure!(
        matches!(url.scheme(), "http" | "https" | "socks5" | "socks5h") && url.host_str().is_some(),
        "Proxy must use HTTP, HTTPS, SOCKS5, or SOCKS5H"
    );
    Ok(url)
}

fn build_client(proxy: Option<&Url>) -> Result<Client> {
    let mut builder = Client::builder()
        .no_proxy()
        .connect_timeout(CONNECT_TIMEOUT)
        .user_agent(concat!("walldl/", env!("CARGO_PKG_VERSION")));
    if let Some(url) = proxy {
        let proxy = Proxy::all(url.as_str()).map_err(|_| anyhow!("Invalid proxy configuration"))?;
        builder = builder.proxy(proxy);
    }
    builder
        .build()
        .map_err(reqwest::Error::without_url)
        .context("Failed to build HTTP client")
}

#[derive(Debug)]
struct Measurement {
    bytes: u64,
    elapsed: Duration,
    first_byte: Duration,
}

impl Measurement {
    fn speed(&self) -> f64 {
        self.bytes as f64 / self.elapsed.as_secs_f64().max(f64::EPSILON)
    }
}

fn partial_length(response: &Response) -> Result<Option<u64>> {
    if response.status() != StatusCode::PARTIAL_CONTENT {
        ensure!(
            matches!(response.status(), StatusCode::OK | StatusCode::NO_CONTENT),
            "Unexpected probe status: {}",
            response.status()
        );
        return Ok(None);
    }
    let value = response
        .headers()
        .get(header::CONTENT_RANGE)
        .context("Missing Content-Range")?
        .to_str()?;
    let (range, total) = value
        .strip_prefix("bytes ")
        .and_then(|value| value.split_once('/'))
        .context("Invalid Content-Range")?;
    let (start, end) = range.split_once('-').context("Invalid byte range")?;
    let start: u64 = start.parse()?;
    let end: u64 = end.parse()?;
    ensure!(
        start == 0 && end < PROBE_BYTES,
        "Unexpected probe byte range"
    );
    if total != "*" {
        ensure!(total.parse::<u64>()? > end, "Invalid range total");
    }
    let length = end + 1;
    if let Some(actual) = response.content_length() {
        ensure!(actual == length, "Content-Length disagrees with byte range");
    }
    Ok(Some(length))
}

async fn probe(client: &Client, url: &Url, budget: Duration) -> Result<Measurement> {
    let started = Instant::now();
    let deadline = started + budget;
    let mut response = timeout_at(
        deadline,
        client
            .get(url.clone())
            .header(header::RANGE, format!("bytes=0-{}", PROBE_BYTES - 1))
            .send(),
    )
    .await
    .context("Probe timed out before response")?
    .map_err(reqwest::Error::without_url)?;

    // Some servers reject Range, including valid requests for empty resources.
    if matches!(
        response.status(),
        StatusCode::BAD_REQUEST | StatusCode::RANGE_NOT_SATISFIABLE | StatusCode::NOT_IMPLEMENTED
    ) {
        response = timeout_at(deadline, client.get(url.clone()).send())
            .await
            .context("Probe retry timed out")?
            .map_err(reqwest::Error::without_url)?;
    }
    response = response
        .error_for_status()
        .map_err(reqwest::Error::without_url)?;
    let expected = partial_length(&response)?;
    let mut bytes = 0;
    let mut first_byte = None;
    loop {
        let chunk = match timeout_at(deadline, response.chunk()).await {
            Ok(result) => result.map_err(reqwest::Error::without_url)?,
            Err(_) if bytes > 0 => break,
            Err(error) => return Err(error).context("Probe timed out without data"),
        };
        let Some(chunk) = chunk else {
            if let Some(expected) = expected {
                ensure!(bytes == expected, "Incomplete partial response");
            }
            break;
        };
        if !chunk.is_empty() {
            first_byte.get_or_insert_with(|| started.elapsed());
            bytes += (chunk.len() as u64).min(PROBE_BYTES - bytes);
            if let Some(expected) = expected {
                ensure!(bytes <= expected, "Partial response exceeds its byte range");
            }
        }
        if bytes == PROBE_BYTES {
            break;
        }
    }
    Ok(Measurement {
        bytes,
        elapsed: started.elapsed(),
        first_byte: first_byte.unwrap_or_else(|| started.elapsed()),
    })
}

fn select_proxy(direct: &Result<Measurement>, proxy: &Result<Measurement>) -> Result<bool> {
    match (direct, proxy) {
        (Ok(direct), Ok(proxy)) => {
            if direct.bytes == 0 || proxy.bytes == 0 {
                ensure!(
                    direct.bytes == proxy.bytes,
                    "Routes disagree on whether the resource is empty"
                );
                Ok(proxy.elapsed < direct.elapsed)
            } else {
                Ok(proxy.speed() > direct.speed())
            }
        }
        (Ok(_), Err(_)) => Ok(false),
        (Err(_), Ok(_)) => Ok(true),
        (Err(direct), Err(proxy)) => {
            bail!("Both probes failed; direct: {direct:#}; proxy: {proxy:#}")
        }
    }
}

async fn choose<'a>(direct: &'a Client, proxy: &'a Client, url: &Url) -> Result<&'a Client> {
    println!("Testing direct and proxy routes...");
    let (direct_result, proxy_result) = tokio::join!(
        probe(direct, url, PROBE_TIMEOUT),
        probe(proxy, url, PROBE_TIMEOUT)
    );
    for (name, result) in [("Direct", &direct_result), ("Proxy", &proxy_result)] {
        match result {
            Ok(sample) => println!(
                "{name}: {:.2} MiB/s, first byte {:.2}s, sampled {} bytes",
                sample.speed() / 1024.0 / 1024.0,
                sample.first_byte.as_secs_f64(),
                sample.bytes
            ),
            Err(error) => eprintln!("{name} probe failed: {error:#}"),
        }
    }
    let use_proxy = select_proxy(&direct_result, &proxy_result)?;
    let reason = if direct_result.is_err() {
        "direct probe failed"
    } else if proxy_result.is_err() {
        "proxy probe failed"
    } else if use_proxy {
        "proxy was faster"
    } else {
        "direct was faster or tied"
    };
    println!(
        "Selected {} ({reason}).",
        if use_proxy { "proxy" } else { "direct" },
    );
    Ok(if use_proxy { proxy } else { direct })
}

async fn download(client: &Client, url: &Url, path: &Path) -> Result<()> {
    let mut response = timeout(IDLE_TIMEOUT, client.get(url.clone()).send())
        .await
        .context("Timed out waiting for download response")?
        .map_err(reqwest::Error::without_url)?
        .error_for_status()
        .map_err(reqwest::Error::without_url)?;
    ensure!(
        matches!(response.status(), StatusCode::OK | StatusCode::NO_CONTENT),
        "Expected a full download response, received {}",
        response.status()
    );
    let size = response.content_length();
    let pb = match size {
        Some(size) => ProgressBar::new(size),
        None => ProgressBar::new_spinner(),
    };
    let template = if size.is_some() {
        "{spinner:.green} [{elapsed_precise}] [{bar:40.cyan/blue}] {bytes}/{total_bytes} ({eta})"
    } else {
        "{spinner:.green} [{elapsed_precise}] {bytes}"
    };
    pb.set_style(ProgressStyle::with_template(template)?.progress_chars("#>-"));
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let parent = parent.to_path_buf();
    let temporary = tokio::task::spawn_blocking(move || NamedTempFile::new_in(parent))
        .await?
        .context("Failed to create temporary output")?;
    let file = fs::File::from_std(
        temporary
            .reopen()
            .context("Failed to open temporary output")?,
    );
    let mut writer = BufWriter::new(file);
    let result: Result<()> = async {
        let mut downloaded = 0;
        while let Some(chunk) = timeout(IDLE_TIMEOUT, response.chunk())
            .await
            .context("Download stalled")?
            .map_err(reqwest::Error::without_url)?
        {
            writer
                .write_all(&chunk)
                .await
                .context("Failed to write download")?;
            downloaded += chunk.len() as u64;
            pb.set_position(downloaded);
        }
        if let Some(size) = size {
            ensure!(downloaded == size, "Incomplete download");
        }
        writer.flush().await.context("Failed to flush output")?;
        writer
            .get_ref()
            .sync_all()
            .await
            .context("Failed to sync output")?;
        Ok(())
    }
    .await;
    drop(writer);
    if let Err(error) = result {
        pb.abandon_with_message("Download failed");
        temporary
            .close()
            .context("Failed to remove partial download")
            .map_err(|cleanup| anyhow!("{error:#}; {cleanup:#}"))?;
        return Err(error);
    }
    let path = path.to_path_buf();
    tokio::task::spawn_blocking(move || temporary.persist(path).map_err(|error| error.error))
        .await?
        .context("Failed to replace destination")?;
    pb.finish_with_message("Download completed");
    Ok(())
}

pub async fn download_file(url: &Url, path: &Path) -> Result<()> {
    let proxy_url = get_proxy_url().await?;
    let direct = build_client(None)?;
    let proxy = build_client(Some(&proxy_url))?;
    let client = choose(&direct, &proxy, url).await?;
    download(client, url, path).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::{io::AsyncReadExt, net::TcpListener, task::JoinHandle};

    struct Server {
        url: Url,
        task: JoinHandle<()>,
    }

    impl Drop for Server {
        fn drop(&mut self) {
            self.task.abort();
        }
    }

    async fn serve(response: Vec<u8>, stall: bool) -> Server {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = Url::parse(&format!("http://{}/file", listener.local_addr().unwrap())).unwrap();
        let task = tokio::spawn(async move {
            loop {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut request = Vec::new();
                while !request.ends_with(b"\r\n\r\n") {
                    let byte = socket.read_u8().await.unwrap();
                    request.push(byte);
                    assert!(request.len() < 16 * 1024);
                }
                // Byte-limited probes deliberately close the connection early.
                let _ = socket.write_all(&response).await;
                if stall {
                    std::future::pending::<()>().await;
                }
            }
        });
        Server { url, task }
    }

    #[tokio::test]
    async fn saves_unknown_length_empty_and_shorter_files() {
        for response in [
            b"HTTP/1.1 200 OK\r\nConnection: close\r\n\r\nnew".to_vec(),
            b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n".to_vec(),
            b"HTTP/1.1 204 No Content\r\n\r\n".to_vec(),
        ] {
            let server = serve(response.clone(), false).await;
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join("output");
            fs::write(&path, "old contents are longer").await.unwrap();
            download(&build_client(None).unwrap(), &server.url, &path)
                .await
                .unwrap();
            let expected = if response.ends_with(b"new") {
                b"new".as_slice()
            } else {
                b""
            };
            assert_eq!(fs::read(path).await.unwrap(), expected);
            assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 1);
        }
    }

    #[tokio::test]
    async fn errors_preserve_existing_output_and_remove_partial_files() {
        for response in [
            b"HTTP/1.1 404 Not Found\r\nContent-Length: 3\r\n\r\nbad".to_vec(),
            b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\n\r\nshort".to_vec(),
            b"HTTP/1.1 206 Partial Content\r\nContent-Length: 3\r\n\r\nbad".to_vec(),
        ] {
            let server = serve(response, false).await;
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join("output");
            fs::write(&path, "keep").await.unwrap();
            assert!(download(&build_client(None).unwrap(), &server.url, &path)
                .await
                .is_err());
            assert_eq!(fs::read(path).await.unwrap(), b"keep");
            assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 1);
        }
    }

    #[tokio::test]
    async fn probes_bound_ignored_ranges_and_handle_partial_or_empty_responses() {
        let mut response = format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n",
            PROBE_BYTES * 2
        )
        .into_bytes();
        response.extend(vec![b'x'; PROBE_BYTES as usize]);
        let server = serve(response, true).await;
        let sample = timeout(
            Duration::from_secs(5),
            probe(&build_client(None).unwrap(), &server.url, PROBE_TIMEOUT),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(sample.bytes, PROBE_BYTES);

        for (response, expected) in [
            ("HTTP/1.1 206 Partial Content\r\nContent-Range: bytes 0-2/3\r\nContent-Length: 3\r\n\r\nabc", 3),
            ("HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n", 0),
            ("HTTP/1.1 204 No Content\r\n\r\n", 0),
        ] {
            let server = serve(response.as_bytes().to_vec(), false).await;
            assert_eq!(probe(&build_client(None).unwrap(), &server.url, PROBE_TIMEOUT).await.unwrap().bytes, expected);
        }
    }

    #[tokio::test]
    async fn rejects_failed_or_malformed_probes() {
        for response in [
            "HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\n\r\n",
            "HTTP/1.1 206 Partial Content\r\nContent-Length: 3\r\n\r\nabc",
            "HTTP/1.1 206 Partial Content\r\nContent-Range: bytes 1-3/4\r\nContent-Length: 3\r\n\r\nabc",
            "HTTP/1.1 206 Partial Content\r\nContent-Range: bytes 0-2/2\r\nContent-Length: 3\r\n\r\nabc",
            "HTTP/1.1 206 Partial Content\r\nContent-Range: bytes 0-2/3\r\nContent-Length: 2\r\n\r\nab",
        ] {
            let server = serve(response.as_bytes().to_vec(), false).await;
            assert!(probe(&build_client(None).unwrap(), &server.url, PROBE_TIMEOUT).await.is_err());
        }
    }

    #[tokio::test]
    async fn bounds_stalled_probes() {
        let server = serve(Vec::new(), true).await;
        let client = build_client(None).unwrap();
        assert!(probe(&client, &server.url, Duration::from_millis(50))
            .await
            .is_err());

        let server = serve(
            b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\n\r\nabc".to_vec(),
            true,
        )
        .await;
        let sample = probe(&client, &server.url, Duration::from_millis(100))
            .await
            .unwrap();
        assert_eq!(sample.bytes, 3);
    }

    #[tokio::test]
    async fn retries_rejected_ranges_without_range_header() {
        for status in [
            "400 Bad Request",
            "416 Range Not Satisfiable",
            "501 Not Implemented",
        ] {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let url =
                Url::parse(&format!("http://{}/file", listener.local_addr().unwrap())).unwrap();
            let task = tokio::spawn(async move {
                for (index, status) in [status, "200 OK"].into_iter().enumerate() {
                    let (mut socket, _) = listener.accept().await.unwrap();
                    let mut request = Vec::new();
                    while !request.ends_with(b"\r\n\r\n") {
                        request.push(socket.read_u8().await.unwrap());
                    }
                    let request = String::from_utf8(request).unwrap().to_ascii_lowercase();
                    assert_eq!(request.contains("\r\nrange:"), index == 0);
                    socket
                        .write_all(
                            format!(
                            "HTTP/1.1 {status}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                        )
                            .as_bytes(),
                        )
                        .await
                        .unwrap();
                }
            });
            let sample = probe(&build_client(None).unwrap(), &url, PROBE_TIMEOUT)
                .await
                .unwrap();
            assert_eq!(sample.bytes, 0);
            task.await.unwrap();
        }
    }

    #[test]
    fn selects_faster_usable_route() {
        let sample = |bytes| {
            Ok(Measurement {
                bytes,
                elapsed: Duration::from_secs(1),
                first_byte: Duration::from_millis(10),
            })
        };
        assert!(select_proxy(&sample(100), &sample(101)).unwrap());
        assert!(!select_proxy(&sample(101), &sample(100)).unwrap());
        assert!(!select_proxy(&sample(100), &sample(100)).unwrap());
        assert!(!select_proxy(&sample(100), &Err(anyhow!("proxy blocked"))).unwrap());
        assert!(select_proxy(&Err(anyhow!("direct blocked")), &sample(100)).unwrap());
        assert!(!select_proxy(&sample(0), &sample(0)).unwrap());
        let faster_empty = Ok(Measurement {
            bytes: 0,
            elapsed: Duration::from_millis(990),
            first_byte: Duration::from_millis(990),
        });
        assert!(select_proxy(&sample(0), &faster_empty).unwrap());
        assert!(!select_proxy(&faster_empty, &sample(0)).unwrap());
        assert!(select_proxy(&sample(0), &sample(100)).is_err());
        let error = select_proxy(
            &Err(anyhow!("direct blocked")),
            &Err(anyhow!("proxy blocked")),
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("direct blocked") && error.contains("proxy blocked"));
    }

    #[tokio::test]
    async fn explicit_proxy_routes_requests_and_errors_do_not_expose_url_credentials() {
        let proxy = serve(
            b"HTTP/1.1 200 OK\r\nContent-Length: 3\r\n\r\nabc".to_vec(),
            false,
        )
        .await;
        let client = build_client(Some(&proxy.url)).unwrap();
        let unreachable = Url::parse("http://nonexistent.invalid/file?secret=value").unwrap();
        assert_eq!(
            probe(&client, &unreachable, PROBE_TIMEOUT)
                .await
                .unwrap()
                .bytes,
            3
        );

        let server = serve(
            b"HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\n\r\n".to_vec(),
            false,
        )
        .await;
        let mut url = server.url.clone();
        url.set_username("private").unwrap();
        url.set_password(Some("password")).unwrap();
        url.set_query(Some("token=secret"));
        let error = probe(&build_client(None).unwrap(), &url, PROBE_TIMEOUT)
            .await
            .unwrap_err();
        let message = format!("{error:#}");
        for secret in ["private", "password", "token=secret"] {
            assert!(!message.contains(secret));
        }
    }

    #[tokio::test]
    async fn supports_socks5_and_socks5h_dns_modes() {
        for (scheme, target, address_type) in [
            ("socks5", "http://127.0.0.1:8081/file", 1),
            ("socks5h", "http://download.invalid:8081/file", 3),
        ] {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let proxy =
                Url::parse(&format!("{scheme}://{}", listener.local_addr().unwrap())).unwrap();
            let task = tokio::spawn(async move {
                let (mut socket, _) = listener.accept().await.unwrap();
                assert_eq!(socket.read_u8().await.unwrap(), 5);
                let count = socket.read_u8().await.unwrap();
                let mut methods = vec![0; count as usize];
                socket.read_exact(&mut methods).await.unwrap();
                assert!(methods.contains(&0));
                socket.write_all(&[5, 0]).await.unwrap();
                let mut connect = [0; 4];
                socket.read_exact(&mut connect).await.unwrap();
                assert_eq!(connect, [5, 1, 0, address_type]);
                match address_type {
                    1 => {
                        let mut address = vec![0; 4];
                        socket.read_exact(&mut address).await.unwrap();
                        assert_eq!(address, [127, 0, 0, 1]);
                    }
                    3 => {
                        let length = socket.read_u8().await.unwrap();
                        let mut address = vec![0; length as usize];
                        socket.read_exact(&mut address).await.unwrap();
                        assert_eq!(address, b"download.invalid");
                    }
                    _ => unreachable!(),
                }
                assert_eq!(socket.read_u16().await.unwrap(), 8081);
                socket
                    .write_all(&[5, 0, 0, 1, 127, 0, 0, 1, 0, 0])
                    .await
                    .unwrap();
                let mut request = Vec::new();
                while !request.ends_with(b"\r\n\r\n") {
                    request.push(socket.read_u8().await.unwrap());
                }
                socket
                    .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 3\r\n\r\nabc")
                    .await
                    .unwrap();
            });
            let client = build_client(Some(&proxy)).unwrap();
            assert_eq!(
                probe(&client, &Url::parse(target).unwrap(), PROBE_TIMEOUT)
                    .await
                    .unwrap()
                    .bytes,
                3
            );
            task.await.unwrap();
        }
    }
}
