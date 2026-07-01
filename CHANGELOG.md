# Changelog

## v0.3.3 (2026-07-01)

- Added `--keep-names` flag to `record` command to preserve the original file names of playlists and segments in the recording. (This may not be compatible with all streams.) ([#6](https://github.com/THEOplayer/streamrr/pull/6))
- Fixed an issue where HLS playlists that start with a [UTF-8 byte order mark (BOM)](https://en.wikipedia.org/wiki/Byte_order_mark#UTF-8) were not parsed correctly. ([#7](https://github.com/THEOplayer/streamrr/issues/7), [#8](https://github.com/THEOplayer/streamrr/pull/8)) 

## v0.3.2 (2026-03-19)

- Added `--header`/`-H` flag to `record` command to add extra HTTP request headers to every request. ([#5](https://github.com/THEOplayer/streamrr/pull/5))

## v0.3.1 (2026-03-13)

- Added support for Windows ARM64.

## v0.3.0 (2026-03-13)

- Added SOCKS proxy support. See [the reqwest docs](https://docs.rs/reqwest/latest/reqwest/#proxies) for usage instructions. ([#3](https://github.com/THEOplayer/streamrr/pull/3))
- Added cookies support while recording. ([#4](https://github.com/THEOplayer/streamrr/pull/4))

## v0.2.0 (2025-11-19)

- Added `--address` flag to `replay` command to set the local server's IP address.
- Fixed an issue where `#EXT-X-MAP` and `#EXT-X-KEY` files were re-downloaded when they re-appear on a later segment.
- The `record` command will now exit gracefully when stopped using Ctrl/Cmd-C.

## v0.1.0 (2025-10-22)

- Initial release.
