use crate::record::{DownloadProgress, RecordError, Source, WithSize};
use crate::shared::{ByteRange, Recording};
use futures::TryStreamExt;
use relative_path::{RelativePath, RelativePathBuf};
use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};
use std::pin::pin;
use std::sync::Arc;
use tokio::fs;
use tokio::io::{AsyncSeekExt, AsyncWriteExt};
use tokio::sync::Mutex;
use tokio_util::bytes::Bytes;
use tokio_util::io::StreamReader;
use tokio_util::sync::CancellationToken;
use url::Url;

/// The output a [`Recorder`](crate::record::Recorder) writes to.
pub trait Storage: Clone + Send + Sync + 'static {
    /// Write a playlist at the given recording-relative path.
    fn write_playlist(
        &self,
        path: &RelativePath,
        contents: Vec<u8>,
    ) -> impl Future<Output = io::Result<()>> + Send;

    /// Store the resource at `url` and `byte_range` under the
    /// recording-relative `path`, unless something is already stored at `path`.
    fn store_resource<S: Source>(
        &self,
        source: &S,
        url: &Url,
        byte_range: Option<ByteRange>,
        download: &dyn DownloadProgress,
        path: &RelativePath,
        token: CancellationToken,
    ) -> impl Future<Output = Result<(), RecordError>> + Send;

    /// Save the recording index.
    fn save_recording(&self, recording: &Recording) -> impl Future<Output = io::Result<()>> + Send;
}

/// Stores a recording as files in a directory on disk.
/// Nothing is created until the first write.
#[derive(Clone)]
pub struct FsStorage {
    dest: PathBuf,
    recording_file: Arc<Mutex<Option<fs::File>>>,
}

impl FsStorage {
    pub fn new(dest: &Path) -> Self {
        Self {
            dest: dest.to_path_buf(),
            recording_file: Arc::new(Mutex::new(None)),
        }
    }
}

impl Storage for FsStorage {
    async fn write_playlist(&self, path: &RelativePath, contents: Vec<u8>) -> io::Result<()> {
        let path = path.to_path(&self.dest);
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
        download: &dyn DownloadProgress,
        path: &RelativePath,
        token: CancellationToken,
    ) -> Result<(), RecordError> {
        let path = path.to_path(&self.dest);
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
            Err(err) if err.kind() == io::ErrorKind::AlreadyExists => {
                download.finish();
                return Ok(());
            }
            Err(err) => return Err(err.into()),
        };
        let WithSize { size, contents } = token
            .run_until_cancelled(source.request_stream(url, byte_range))
            .await
            .ok_or(RecordError::Cancelled)?;
        if let Some(size) = size {
            download.set_size(size);
        }
        let contents = contents
            .inspect_ok(|chunk| download.inc(chunk.len() as u64))
            .map_err(io::Error::other);
        let mut response_stream = pin!(StreamReader::new(contents));
        tokio::io::copy_buf(&mut response_stream, &mut file).await?;
        download.finish();
        Ok(())
    }

    async fn save_recording(&self, recording: &Recording) -> io::Result<()> {
        let recording_json = serde_json::to_string_pretty(recording)?;
        let recording_bytes = recording_json.as_bytes();
        let mut recording_file = self.recording_file.lock().await;
        let file = match recording_file.as_mut() {
            Some(file) => file,
            None => {
                fs::create_dir_all(&self.dest).await?;
                let file = fs::File::create(self.dest.join("recording.json")).await?;
                recording_file.insert(file)
            }
        };
        file.rewind().await?;
        file.write_all(recording_bytes).await?;
        file.set_len(recording_bytes.len() as u64).await?;
        Ok(())
    }
}

/// Stores a recording in memory, for replaying without writing to disk.
#[derive(Clone, Default)]
pub struct MemoryStorage {
    inner: Arc<std::sync::Mutex<MemoryRecording>>,
}

/// The contents of a recording held in memory.
#[derive(Default)]
pub struct MemoryRecording {
    pub recording: Recording,
    /// Playlist contents by recording-relative path.
    pub playlists: HashMap<RelativePathBuf, Bytes>,
    /// Resources by path, fetched from the source only when requested.
    pub resources: HashMap<RelativePathBuf, Resource>,
}

/// A resource to be fetched from a source on demand.
#[derive(Clone, Debug)]
pub struct Resource {
    pub url: Url,
    pub byte_range: Option<ByteRange>,
}

impl MemoryStorage {
    /// Take the recorded contents out of the storage.
    pub fn take(&self) -> MemoryRecording {
        std::mem::take(&mut *self.inner.lock().unwrap())
    }
}

impl Storage for MemoryStorage {
    async fn write_playlist(&self, path: &RelativePath, contents: Vec<u8>) -> io::Result<()> {
        self.inner
            .lock()
            .unwrap()
            .playlists
            .insert(path.to_owned(), Bytes::from(contents));
        Ok(())
    }

    async fn store_resource<S: Source>(
        &self,
        _source: &S,
        url: &Url,
        byte_range: Option<ByteRange>,
        download: &dyn DownloadProgress,
        path: &RelativePath,
        _token: CancellationToken,
    ) -> Result<(), RecordError> {
        download.finish();
        self.inner
            .lock()
            .unwrap()
            .resources
            .entry(path.to_owned())
            .or_insert(Resource {
                url: url.clone(),
                byte_range,
            });
        Ok(())
    }

    async fn save_recording(&self, recording: &Recording) -> io::Result<()> {
        self.inner.lock().unwrap().recording = recording.clone();
        Ok(())
    }
}
