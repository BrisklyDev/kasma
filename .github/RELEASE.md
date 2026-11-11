## Kasma 0.1.0

The first release of Kasma, a multi-connection download accelerator for the command line.

### :rocket: Highlights

- **Parallel downloads:** splits a file into byte ranges and downloads them over up to 64 connections (default 8).
- **Dynamic range splitting:** when a connection finishes early, it takes over part of a slower connection's remaining range, so all connections stay busy until the end.
- **Pause and resume:** press `p` or `Space` to pause. Press `q` or `Ctrl+C` to quit. Run the same command again and the download continues where it left off.
- **Stall recovery:** connections that receive no data for `--timeout` seconds are reset and retried, up to `--retries` times (or `inf`).
- **Speed limit:** cap the total download speed with `-l`, e.g. `-l 2M`.
- **Single-connection fallback:** servers without range request support still work, over one connection.
- **HTTPS** via rustls, using the system's root certificates.
