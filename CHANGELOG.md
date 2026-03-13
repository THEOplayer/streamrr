# Changelog

## v0.3.0 (2026-03-13)

- Added SOCKS proxy support. See [the reqwest docs](https://docs.rs/reqwest/latest/reqwest/#proxies) for usage instructions. ([#3](https://github.com/THEOplayer/streamrr/pull/3))
- Added cookies support while recording. ([#4](https://github.com/THEOplayer/streamrr/pull/4))

## v0.2.0 (2025-11-19)

- Added `--address` flag to `replay` command to set the local server's IP address.
- Fixed an issue where `#EXT-X-MAP` and `#EXT-X-KEY` files were re-downloaded when they re-appear on a later segment.
- The `record` command will now exit gracefully when stopped using Ctrl/Cmd-C.

## v0.1.0 (2025-10-22)

- Initial release.
