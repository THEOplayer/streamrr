use crate::record::{RecordError, Source};
use crate::shared::{ByteRange, Recording};
use futures::TryStreamExt;
use std::io;
use std::path::{Path, PathBuf};
use std::pin::pin;
use std::sync::Arc;
use tokio::fs;
use tokio::io::{AsyncSeekExt, AsyncWriteExt};
use tokio::sync::Mutex;
use tokio_util::io::StreamReader;
use tokio_util::sync::CancellationToken;
use url::Url;

/// The output a [`Recorder`](crate::record::Recorder) writes to.
pub trait Storage: Clone + Send + Sync + 'static {
    /// Write a playlist at the given recording-relative path (`/`-separated).
    fn write_playlist(
        &self,
        path: &str,
        contents: Vec<u8>,
    ) -> impl Future<Output = io::Result<()>> + Send;

    /// Store the resource at `url` and `byte_range` under the
    /// recording-relative `path`, unless something is already stored at `path`.
    fn store_resource<S: Source>(
        &self,
        source: &S,
        url: &Url,
        byte_range: Option<ByteRange>,
        path: &str,
        token: CancellationToken,
    ) -> impl Future<Output = Result<(), RecordError>> + Send;

    /// Save the recording index.
    fn save_recording(&self, recording: &Recording) -> impl Future<Output = io::Result<()>> + Send;
}

/// Stores a recording as files in a directory on disk.
#[derive(Clone)]
pub struct FsStorage {
    dest: PathBuf,
    recording_file: Arc<Mutex<fs::File>>,
}

impl FsStorage {
    pub async fn new(dest: &Path) -> io::Result<Self> {
        fs::create_dir_all(dest).await?;
        let file = fs::File::create(dest.join("recording.json")).await?;
        Ok(Self {
            dest: dest.to_path_buf(),
            recording_file: Arc::new(Mutex::new(file)),
        })
    }
}

impl Storage for FsStorage {
    async fn write_playlist(&self, path: &str, contents: Vec<u8>) -> io::Result<()> {
        let path = self.dest.join(path);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).await?;
        }
        fs::write(path, contents).await
    }

    async fn store_resource<S: Source>(
        &self,
        source: &S,
        url: &Url,
        byte_range: Option<ByteRange>,
        path: &str,
        token: CancellationToken,
    ) -> Result<(), RecordError> {
        let path = self.dest.join(path);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).await?;
        }
        let mut file = match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .await
        {
            Ok(file) => file,
            Err(err) if err.kind() == io::ErrorKind::AlreadyExists => return Ok(()),
            Err(err) => return Err(err.into()),
        };
        let response_stream = token
            .run_until_cancelled(source.request_stream(url, byte_range))
            .await
            .ok_or(RecordError::Cancelled)?
            .map_err(io::Error::other);
        let mut response_stream = pin!(StreamReader::new(response_stream));
        tokio::io::copy_buf(&mut response_stream, &mut file).await?;
        Ok(())
    }

    async fn save_recording(&self, recording: &Recording) -> io::Result<()> {
        let recording_json = serde_json::to_string_pretty(recording)?;
        let recording_bytes = recording_json.as_bytes();
        let mut file = self.recording_file.lock().await;
        file.rewind().await?;
        file.write_all(recording_bytes).await?;
        file.set_len(recording_bytes.len() as u64).await?;
        Ok(())
    }
}
