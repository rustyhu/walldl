# Plan

## Scope

- Always measure direct and proxy routes to select the faster usable connection.
- Support HTTP/HTTPS and SOCKS5 proxies for slow or blocked destinations.
- Keep single-stream downloads; no concurrent or segmented downloading currently.
- Use the only successful route; if both probes fail, report both errors and stop.

## Improvements

- Download: validate HTTP status, return non-zero on failure, and support unknown content lengths.
- Files: download to a temporary file, replace the destination only on success, and define cleanup behavior.
- CLI: validate arguments and derive filenames using URL parsing.
- Proxy: use explicit clients, disable environment proxies for direct access, redact credentials, and document SOCKS DNS behavior.
- Configuration: preserve `proxy.txt` > `PROXY` > localhost default; report invalid or unreadable configuration.
- Clients: reuse the selected client; separate connection timeout, probe deadline, and download idle timeout.
- Probes: bound time and bytes, validate partial responses, and handle ignored Range, small files, and empty responses.
- Selection: choose the faster measured route without a preference threshold; use direct only for exact ties.
- Reporting: show measurements and selection reasons; do not silently switch routes mid-download.
- Dependencies: retain the current stack, keep `stream`, enable `socks`, remove unused `json`, and narrow Tokio features.

## Next feature

- Add single-stream resume using HTTP Range requests, preserving partial downloads for later continuation.
- Validate the remote resource and range response before appending; handle changed resources and servers that ignore or reject Range without corrupting the output.

## Validation and order

- Add local-server tests for route failures, proxy isolation, Range handling, timeouts, file preservation, and exit status.
- Implement download correctness first, then proxy/client handling, then improved probes; add tests alongside each change.