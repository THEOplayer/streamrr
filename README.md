# Stream Record & Replay

Record HLS streams and replay them afterwards.

## Installation

Prerequisites:

1. [Install Rust](https://www.rust-lang.org/tools/install)

```bash
cargo install --git https://github.com/THEOplayer/streamrr.git
```

This will build the `streamrr` CLI tool, and then add it to your `$PATH`.
If everything goes well, you should now be able to run:

```bash
streamrr --help
```

This will print the usage instructions.

## Usage

The CLI has two main commands: `record` and `replay`.

### Recording

`streamrr record` will record an HLS VOD or live stream to a directory on your local disk.

```bash
streamrr record https://example.com/mystream.m3u8 recordings/mystream/
```

This will start recording the first variant stream of the HLS master playlist, along with all it segments.
If it's an HLS live stream, the tool will repeatedly fetch the latest playlist, and download all newly added segments.

Run `streamrr record --help` for the full instructions.

### Replaying

`streamrr replay` will spawn a local HTTP server and serve a previously recorded stream.

```bash
streamrr replay recordings/mystream/
```

The replayed stream is available at `http://localhost:8080/index.m3u8`.

* If it's a recording of an HLS live stream, then the server will also replay all downloaded playlists as they appear in
  the recording. Any HLS player playing this replayed stream will see the same sequence of playlists, and will behave as
  if the stream was truly "live".
* If it's a recording of an HLS VOD stream, then the server will simply serve all files. (The tool doesn't do anything
  special in this case, you could also put the recorded files on any static web server.)

Run `streamrr replay --help` for the full instructions.

## Sharing recordings

All files necessary to replay the stream are saved directly to the given recording directory. You can easily share these
recordings with others by putting them in an archive:

```bash
tar -cvf myrecording.tar recordings/mystream/
```

## Developing

While working on streamrr itself, you can use `cargo run` instead of `streamrr` to build and run the tool.

To pass parameters to the CLI, put a `--` between the `cargo run` command and the actual parameters. For example:

```bash
cargo run -- record https://example.com/mystream.m3u8 recordings/mystream/
```

To build in release mode, use `cargo run --release`.

To install your local copy as `streamrr`:

```bash
cd streamrr
cargo install --path .
```

## Third-party dependencies

We use [`cargo-about`](https://github.com/EmbarkStudios/cargo-about/) to maintain the list of open-source licenses
of our third-party dependencies.

Whenever you add or update a dependency to `Cargo.toml`, run:

```bash
cargo install --locked cargo-about
cargo about generate NOTICE.hbs -o NOTICE.md
```
