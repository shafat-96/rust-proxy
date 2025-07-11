# M3u8 proxy written in rust

A fast, no-caching proxy server for `.m3u8` HLS playlists and segments, built using Rust and Actix-Web.

It rewrites `.m3u8` files so that all segment requests (like `.ts`, `.vtt`, etc.) go through the same proxy — enabling CORS and header manipulation.


- Streams `.m3u8`, `.ts`, `.vtt`, etc.
- Supports custom headers via `?headers=...`
- Handles CORS automatically
- Fast: uses keep-alive connection pooling


## Requirements

> If you don't have Rust/Cargo installed:

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
```

## Running the Server

```bash
cargo run

```

The server will start at (change port from `main.rs` if you want to):

```
http://127.0.0.1:8080

```

## API Usage

### Proxy a direct file or media segment

```
GET /?url=https://example.com/file.ts
```

### Proxy a .m3u8 playlist and rewrite internal URLs

```
GET /?url=https://example.com/playlist.m3u8
```

### Proxy with headers (JSON string, URL encoded if needed)

```
GET /?url=https://example.com/playlist.m3u8&headers={"Referer":"https://example.com"}
```

---

## LICENSE

Using: [Apache License 2.0](LICENSE)

## Credits

Inspired by: https://github.com/Gratenes/m3u8CloudflareWorkerProxy