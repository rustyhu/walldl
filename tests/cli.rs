use std::{process::Output, time::Duration};

use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    process::Command,
};

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
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/file", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        loop {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            while !request.ends_with(b"\r\n\r\n") {
                request.push(socket.read_u8().await.unwrap());
            }
            let is_probe = String::from_utf8_lossy(&request)
                .to_ascii_lowercase()
                .contains("\r\nrange:");
            let request_text = String::from_utf8_lossy(&request);
            let target = request_text.split_whitespace().nth(1).unwrap();
            let fail_transfer = target.ends_with("/fail");
            let response = if fail_transfer && !is_probe {
                b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\nConnection: close\r\n\r\nshort"
                    .as_slice()
            } else {
                b"HTTP/1.1 200 OK\r\nContent-Length: 3\r\nConnection: close\r\n\r\nnew".as_slice()
            };
            socket.write_all(response).await.unwrap();
        }
    });
    let directory = tempfile::tempdir().unwrap();
    std::fs::write(directory.path().join("proxy.txt"), format!("  {url}\n")).unwrap();
    let success = run(directory.path(), &[&url, "-o", "output"], "invalid").await;
    assert!(
        success.status.success(),
        "{}",
        String::from_utf8_lossy(&success.stderr)
    );
    let log = String::from_utf8_lossy(&success.stdout);
    assert!(log.contains("Direct:") && log.contains("Proxy:"));
    assert_eq!(
        std::fs::read(directory.path().join("output")).unwrap(),
        b"new"
    );
    std::fs::write(directory.path().join("proxy.txt"), "").unwrap();
    let proxy_failure = run(
        directory.path(),
        &[&url, "-o", "output"],
        "http://127.0.0.1:1",
    )
    .await;
    assert!(proxy_failure.status.success());
    assert!(String::from_utf8_lossy(&proxy_failure.stdout).contains("Selected direct"));
    let direct_failure = run(
        directory.path(),
        &["http://127.0.0.1:1/file", "-o", "output"],
        &url,
    )
    .await;
    assert!(direct_failure.status.success());
    assert!(String::from_utf8_lossy(&direct_failure.stdout).contains("Selected proxy"));

    let failed_url = url.replace("/file", "/fail");
    let failed_download = run(directory.path(), &[&failed_url, "-o", "output"], &url).await;
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
    assert!(error.contains("Both probes failed"));
    assert!(!String::from_utf8_lossy(&failure.stdout).contains("Done!"));
    server.abort();
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
