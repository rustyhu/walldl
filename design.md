# Design

- When able to know the downloading file size, small files (up to 4 MiB): try proxy access first; if that route fails, try direct access. Skip speed comparison.
- Larger files or unknown sizes: compare direct and proxy speeds and choose the faster usable route. Exact ties use direct.

# fatal error
- If neither route works, report both failures and stop.

# future
- Current downloads are single-stream; consider concurrent or segmented downloading in next stage.
