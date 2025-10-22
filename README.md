# streamrr

Record HLS streams and replay them afterwards.

## Installation

Download the [latest version from GitHub](https://github.com/THEOplayer/streamrr/releases/latest).

Supported platforms:
- [Windows (x86-64)](https://github.com/THEOplayer/streamrr/releases/download/latest/streamrr-x86_64-windows.exe)
- [Linux (x86-64)](https://github.com/THEOplayer/streamrr/releases/download/latest/streamrr-x86_64-linux-gnu)
- [macOS (Apple Silicon)](https://github.com/THEOplayer/streamrr/releases/download/latest/streamrr-aarch64-macos)
- Is your platform missing? [Let us know.](https://github.com/THEOplayer/streamrr/issues)

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
