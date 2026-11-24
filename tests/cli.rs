use std::{
    process::Output,
    sync::{Arc, Mutex},
    time::Duration,
};

use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    process::Command,
    task::{JoinHandle, JoinSet},
};

struct Server {
    url: String,
    requests: Arc<Mutex<Vec<String>>>,
    task: JoinHandle<()>,
}

impl Drop for Server {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl Server {
    async fn new(respond: impl Fn(&str) -> Vec<u8> + Send + Sync + 'static) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/file", listener.local_addr().unwrap());
        let requests = Arc::new(Mutex::new(Vec::new()));
        let recorded = requests.clone();
        let respond = Arc::new(respond);
        let task = tokio::spawn(async move {
            let mut connections = JoinSet::new();
            loop {
                tokio::select! {
                    accepted = listener.accept() => {
                        let (mut socket, _) = accepted.unwrap();
                        let recorded = recorded.clone();
                        let respond = respond.clone();
                        connections.spawn(async move {
                            let mut request = Vec::new();
                            while !request.ends_with(b"\r\n\r\n") {
                                request.push(socket.read_u8().await.unwrap());
                                assert!(request.len() < 16 * 1024);
                            }
                            let request = String::from_utf8(request).unwrap();
                            recorded.lock().unwrap().push(request.clone());
                            // A completed probe can close a response before the server finishes writing.
                            let _ = socket.write_all(&respond(&request)).await;
                        });
                    }
                    Some(result) = connections.join_next(), if !connections.is_empty() => {
                        result.unwrap();
                    }
                }
            }
        });
        Self {
            url,
            requests,
            task,
        }
    }
}

async fn run(directory: &std::path::Path, args: &[&str], proxy: &str) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_walldl"));
    command
        .args(args)
        .current_dir(directory)
        .env("PROXY", proxy)
        .env("HTTP_PROXY", "http://127.0.0.1:1")
        .env("http_proxy", "http://127.0.0.1:1")
        .env("HTTPS_PROXY", "http://127.0.0.1:1")
        .env("https_proxy", "http://127.0.0.1:1")
        .env("ALL_PROXY", "http://127.0.0.1:1")
        .env("all_proxy", "http://127.0.0.1:1")
        .env("NO_PROXY", "")
        .env("no_proxy", "")
        .kill_on_drop(true);
    tokio::time::timeout(Duration::from_secs(20), command.output())
        .await
        .unwrap()
        .unwrap()
}

#[tokio::test]
async fn respects_file_proxy_and_direct_isolation_and_reports_failed_routes() {
    let server = Server::new(|request| {
        let target = request.split_whitespace().nth(1).unwrap();
        let response = if target.ends_with("/fail") {
            b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\nConnection: close\r\n\r\nshort".as_slice()
        } else {
            b"HTTP/1.1 200 OK\r\nContent-Length: 3\r\nConnection: close\r\n\r\nnew".as_slice()
        };
        response.to_vec()
    })
    .await;
    let url = &server.url;
    let directory = tempfile::tempdir().unwrap();
    std::fs::write(directory.path().join("proxy.txt"), format!("  {url}\n")).unwrap();
    let success = run(directory.path(), &[url, "-o", "output"], "invalid").await;
    assert!(
        success.status.success(),
        "{}",
        String::from_utf8_lossy(&success.stderr)
    );
    let log = String::from_utf8_lossy(&success.stdout);
    assert!(log.contains("Selected proxy") && !log.contains("Testing direct"));
    assert_eq!(server.requests.lock().unwrap().len(), 1);
    assert_eq!(
        std::fs::read(directory.path().join("output")).unwrap(),
        b"new"
    );
    std::fs::write(directory.path().join("proxy.txt"), "").unwrap();
    let proxy_failure = run(
        directory.path(),
        &[url, "-o", "output"],
        "http://127.0.0.1:1",
    )
    .await;
    assert!(proxy_failure.status.success());
    assert!(String::from_utf8_lossy(&proxy_failure.stdout).contains("Selected direct"));
    let direct_failure = run(
        directory.path(),
        &["http://127.0.0.1:1/file", "-o", "output"],
        url,
    )
    .await;
    assert!(direct_failure.status.success());
    assert!(String::from_utf8_lossy(&direct_failure.stdout).contains("Selected proxy"));

    let failed_url = url.replace("/file", "/fail");
    let failed_download = run(directory.path(), &[&failed_url, "-o", "output"], url).await;
    assert!(!failed_download.status.success());
    assert!(!String::from_utf8_lossy(&failed_download.stdout).contains("Done!"));
    assert_eq!(
        std::fs::read(directory.path().join("output")).unwrap(),
        b"new"
    );
    assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 2);

    let failure = run(
        directory.path(),
        &["http://127.0.0.1:1/file"],
        "http://127.0.0.1:1",
    )
    .await;
    assert!(!failure.status.success());
    let error = String::from_utf8_lossy(&failure.stderr);
    assert!(error.contains("Both routes failed"));
    assert!(!String::from_utf8_lossy(&failure.stdout).contains("Done!"));
}

#[tokio::test]
async fn small_files_including_four_mib_reuse_only_the_proxy_request() {
    for size in [0, 4 * 1024 * 1024] {
        let server = Server::new(move |_| {
            let mut response =
                format!("HTTP/1.1 200 OK\r\nContent-Length: {size}\r\nConnection: close\r\n\r\n")
                    .into_bytes();
            response.extend(vec![b'x'; size]);
            response
        })
        .await;
        let directory = tempfile::tempdir().unwrap();
        let output = run(
            directory.path(),
            &[&server.url, "-o", "output"],
            &server.url,
        )
        .await;
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            std::fs::read(directory.path().join("output")).unwrap(),
            vec![b'x'; size]
        );
        let requests = server.requests.lock().unwrap();
        assert_eq!(requests.len(), 1);
        assert!(requests[0].starts_with("GET http://"));
        assert!(!requests[0].to_ascii_lowercase().contains("\r\nrange:"));
        assert!(!String::from_utf8_lossy(&output.stdout).contains("Testing direct"));
    }
}

#[tokio::test]
async fn larger_and_unknown_files_still_compare_both_routes() {
    for size in [Some(4 * 1024 * 1024 + 1), None] {
        let server = Server::new(move |request| {
            let range = request.to_ascii_lowercase().contains("\r\nrange:");
            let (headers, body_size) = match (size, range) {
                (Some(size), true) => (
                    format!("HTTP/1.1 206 Partial Content\r\nContent-Range: bytes 0-524287/{size}\r\nContent-Length: 524288\r\n"),
                    512 * 1024,
                ),
                (Some(size), false) => (format!("HTTP/1.1 200 OK\r\nContent-Length: {size}\r\n"), size),
                (None, _) => ("HTTP/1.1 200 OK\r\n".into(), 3),
            };
            let mut response = format!("{headers}Connection: close\r\n\r\n").into_bytes();
            response.extend(vec![b'x'; body_size]);
            response
        }).await;
        let directory = tempfile::tempdir().unwrap();
        let output = run(
            directory.path(),
            &[&server.url, "-o", "output"],
            &server.url,
        )
        .await;
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let log = String::from_utf8_lossy(&output.stdout);
        assert!(log.contains("Direct:") && log.contains("Proxy:"));
        assert_eq!(
            std::fs::read(directory.path().join("output")).unwrap(),
            vec![b'x'; size.unwrap_or(3)]
        );
        let requests = server.requests.lock().unwrap();
        assert_eq!(requests.len(), 3);
        assert_eq!(
            requests
                .iter()
                .filter(|request| request.starts_with("GET /"))
                .count(),
            if log.contains("Selected direct") {
                2
            } else {
                1
            }
        );
        assert_eq!(
            requests
                .iter()
                .filter(|request| request.to_ascii_lowercase().contains("\r\nrange:"))
                .count(),
            1
        );
    }
}

#[tokio::test]
async fn small_proxy_transfer_failure_restarts_direct_but_local_errors_do_not() {
    let server = Server::new(|request| {
        if request.starts_with("GET http://") {
            b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\nConnection: close\r\n\r\npartial-proxy"
                .to_vec()
        } else {
            b"HTTP/1.1 200 OK\r\nContent-Length: 6\r\nConnection: close\r\n\r\ndirect".to_vec()
        }
    })
    .await;
    let directory = tempfile::tempdir().unwrap();
    let output = run(
        directory.path(),
        &[&server.url, "-o", "output"],
        &server.url,
    )
    .await;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        std::fs::read(directory.path().join("output")).unwrap(),
        b"direct"
    );
    assert_eq!(server.requests.lock().unwrap().len(), 2);
    assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 1);

    server.requests.lock().unwrap().clear();
    let output = run(
        directory.path(),
        &[&server.url, "-o", "missing/output"],
        &server.url,
    )
    .await;
    assert!(!output.status.success());
    assert_eq!(server.requests.lock().unwrap().len(), 1);
    assert!(!String::from_utf8_lossy(&output.stdout).contains("Selected direct"));
    assert!(String::from_utf8_lossy(&output.stderr).contains("Failed to create temporary output"));
}

#[tokio::test]
async fn rejects_bad_cli_and_configuration_without_revealing_credentials() {
    let directory = tempfile::tempdir().unwrap();
    for args in [
        vec![],
        vec!["https://example.com/file", "-o"],
        vec!["https://example.com/file", "--unknown"],
    ] {
        assert!(!run(directory.path(), &args, "unused")
            .await
            .status
            .success());
    }

    for proxy in [
        "ftp://private:password@localhost",
        "http://private:password@[invalid",
    ] {
        let output = run(directory.path(), &["https://example.com/file"], proxy).await;
        assert!(!output.status.success());
        let error = String::from_utf8_lossy(&output.stderr);
        assert!(!error.contains("private") && !error.contains("password"));
    }

    std::fs::create_dir(directory.path().join("proxy.txt")).unwrap();
    let output = run(directory.path(), &["https://example.com/file"], "unused").await;
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("Failed to read proxy.txt"));
}
