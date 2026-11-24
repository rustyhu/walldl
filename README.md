# walldl

Stream downloader with automatic direct/proxy routing.

```sh
walldl https://example.com/file.zip -o file.zip
```

- Output: omit `-o` to use the URL filename, or `outfile` if absent.
- Proxy: non-empty `proxy.txt` > non-empty `PROXY` > `http://127.0.0.1:8080`.
- Protocols: HTTP/HTTPS, `socks5://` (local DNS), and `socks5h://` (proxy DNS).
- Safety: replace the destination only on success; download failures exit non-zero.

## Routing

- [Design](design.md): files up to 4 MiB use proxy first, then direct on failure; larger or unknown-size files use the faster measured route. Stop if both routes fail.
- Known small files reuse the first proxy GET; a failed proxy transfer restarts through direct access. Local file errors do not trigger route changes.
- Single-stream resume is planned; see [plan.md](plan.md).
