# streamrr

Record HLS streams and replay them afterwards.

## Installation

### Option 1: Install from pre-built release

1. Download the [latest version from GitHub](https://github.com/THEOplayer/streamrr/releases/latest).  
   Supported platforms:
  - [Windows (x86-64)](https://github.com/THEOplayer/streamrr/releases/latest/download/streamrr-x86_64-pc-windows-msvc.zip)
  - [Linux (x86-64)](https://github.com/THEOplayer/streamrr/releases/latest/download/streamrr-x86_64-unknown-linux-gnu.tar.xz)
  - [Linux (ARM64)](https://github.com/THEOplayer/streamrr/releases/latest/download/streamrr-aarch64-unknown-linux-gnu.tar.xz)
  - [macOS (Apple Silicon)](https://github.com/THEOplayer/streamrr/releases/latest/download/streamrr-aarch64-apple-darwin.tar.xz)
  - Is your platform missing? [Let us know.](https://github.com/THEOplayer/streamrr/issues)
2. Extract the archive.
3. Run `streamrr` (or `streamrr.exe` on Windows) from a terminal.
4. (Optional: add `streamrr` to your `$PATH` to run it from anywhere.)

### Option 2: Install from source

1. [Install Rust.](https://www.rust-lang.org/tools/install)
2. Run:
   ```bash
   cargo install --git https://github.com/THEOplayer/streamrr.git
   ```
   This will build the `streamrr` CLI tool, and then add it to your `$PATH`.

## Usage

The `streamrr` CLI has two main commands: `record` and `replay`.

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

### Sharing recordings

All files necessary to replay the stream are saved directly to the given recording directory. You can easily share these
recordings with others by putting them in an archive:

```bash
tar -cvf myrecording.tar recordings/mystream/
```

## License

This software is distributed under the [BSD 3-Clause Clear License](https://spdx.org/licenses/BSD-3-Clause-Clear.html). See [the license file](./LICENSE.md) for more information.
