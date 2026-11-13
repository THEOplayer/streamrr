# Stream Record & Replay

Record HLS streams and replay them afterwards.

[![streamrr recording a stream](assets/record.gif)](https://asciinema.org/a/wuFkMqKDWwa2ygKY)

## Installation

### Option 1: Install from pre-built release

1. Download the [latest version from GitHub](https://github.com/THEOplayer/streamrr/releases/latest).  
   Supported platforms:
   - [Windows (x86-64)](https://github.com/THEOplayer/streamrr/releases/latest/download/streamrr-x86_64-pc-windows-msvc.zip)
   - [Windows (ARM64)](https://github.com/THEOplayer/streamrr/releases/latest/download/streamrr-aarch64-pc-windows-msvc.zip)
   - [Linux (x86-64)](https://github.com/THEOplayer/streamrr/releases/latest/download/streamrr-x86_64-unknown-linux-gnu.tar.xz)
   - [Linux (ARM64)](https://github.com/THEOplayer/streamrr/releases/latest/download/streamrr-aarch64-unknown-linux-gnu.tar.xz)
   - [macOS (Apple Silicon)](https://github.com/THEOplayer/streamrr/releases/latest/download/streamrr-aarch64-apple-darwin.tar.xz)
   - [macOS (x86-64)](https://github.com/THEOplayer/streamrr/releases/latest/download/streamrr-x86_64-apple-darwin.tar.xz)
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

If everything goes well, you should now be able to run:
```bash
streamrr --help
```

This will print the usage instructions.

## Usage

The CLI has three main commands: `record`, `replay` and `import`.

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

`streamrr replay` can also serve an HTTP Archive (HAR) file directly, without importing it first:

```bash
streamrr replay capture.har
```

All captured variants and renditions are served, nothing is written to disk, and segments are decoded from the
HAR only when requested.

Run `streamrr replay --help` for the full instructions.

### Importing

`streamrr import` will import an HTTP Archive (HAR) file as a recording.

In your browser's developer tools, open the *Network* tab while playing the stream, then save all captured
requests as a HAR file. In Chrome and Edge, right-click in the request list and choose *"Save all as HAR with content"*
(or use the *Export HAR* button). In Firefox, right-click and choose *"Save All As HAR"*. Make sure response content
is included.

```bash
streamrr import capture.har recordings/mystream/
```

This will import the first HLS playlist found in the HAR file. To import a different stream, pass its playlist URL
with `--url`:

```bash
streamrr import --url https://example.com/mystream.m3u8 capture.har recordings/mystream/
```

Requests that were not captured (for example, segments that were already cached by the browser) are marked with
`#EXT-X-GAP` in the imported playlists. Low-Latency HLS (LL-HLS) streams are not supported yet.

Run `streamrr import --help` for the full instructions.

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

## Changelog

We use [Changie](https://changie.dev/) to manage the changelog. Don't edit `CHANGELOG.md` directly,
it's generated from the files in `.changes/`.

For every user-facing change, install [Changie v1.26.0](https://github.com/miniscruff/changie/releases/tag/v1.26.0)
and run:

```bash
changie new
```

Write the entry in Markdown, including links to the relevant issues and pull requests,
and commit the generated `.changes/unreleased/*.yaml` file along with your change.
Pull requests without a new changelog entry fail the **Changelog** check,
unless a maintainer applies the `skip-changelog` label.

## Releasing

1. In GitHub Actions, run the **Prepare release** workflow on `main`, choosing a `major`, `minor` or `patch` bump.
   This collects the pending changelog entries into a new version, updates the version in `Cargo.toml`,
   and opens a `release/vX.Y.Z` pull request.
2. Review the release PR, wait for CI to pass, and merge it with **Rebase and merge**.
   That way, the release commit lands on `main` as just `vX.Y.Z`.
3. Merging the release PR runs the **Tag release** workflow, which pushes a `vX.Y.Z` tag.
   That tag triggers the **Release** workflow, which builds the binaries and publishes the GitHub release
   with the notes from `CHANGELOG.md`. Afterwards, the **Release title** job renames the release to just `vX.Y.Z`.

If a release fails before it's published (e.g. a broken build), fix the problem on `main`
and delete the `vX.Y.Z` tag. Then run the **Tag release** workflow on `main` to tag the latest commit again.

## Third-party dependencies

We use [`cargo-about`](https://github.com/EmbarkStudios/cargo-about/) to maintain the list of open-source licenses
of our third-party dependencies.

Whenever you add or update a dependency to `Cargo.toml`, run:

```bash
cargo install --locked cargo-about
cargo about generate NOTICE.hbs -o NOTICE.md
```
