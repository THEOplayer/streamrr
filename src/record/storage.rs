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
        if fs::try_exists(&path).await? {
            download.finish();
            return Ok(());
        }
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).await?;
        }
        let mut part_path = path.as_os_str().to_os_string();
        part_path.push(".part");
        // Removes the partial file on failure or cancellation. Declared before `file`,
        // so the file is closed before it gets removed (required on Windows).
        let temp_path = tempfile::TempPath::try_from_path(PathBuf::from(part_path))?;
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&temp_path)
            .await?;
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
        file.flush().await?;
        file.sync_all().await?;
        drop(file);
        match temp_path.persist_noclobber(&path) {
            Ok(()) => {}
            Err(err) if err.error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(err) => return Err(err.error.into()),
        }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::record::NoProgress;
    use crate::shared::Timed;
    use chrono::{DateTime, Utc};
    use futures::{Stream, stream};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    #[derive(Clone)]
    struct FakeSource {
        chunks: Vec<&'static [u8]>,
        fail: bool,
        requests: Arc<AtomicUsize>,
    }

    impl FakeSource {
        fn new(chunks: Vec<&'static [u8]>, fail: bool) -> Self {
            Self {
                chunks,
                fail,
                requests: Arc::new(AtomicUsize::new(0)),
            }
        }
    }

    impl Source for FakeSource {
        type Error = io::Error;

        async fn wait_for_refresh(&mut self, _: DateTime<Utc>, _: Duration) {}

        async fn request_playlist(&self, _: &Url) -> io::Result<Option<Timed<String>>> {
            unreachable!()
        }

        async fn request(&self, _: &Url, _: Option<ByteRange>) -> io::Result<Timed<Bytes>> {
            unreachable!()
        }

        async fn request_stream(
            &self,
            _: &Url,
            _: Option<ByteRange>,
        ) -> WithSize<impl Stream<Item = io::Result<Bytes>> + Send> {
            self.requests.fetch_add(1, Ordering::SeqCst);
            let mut chunks: Vec<_> = self
                .chunks
                .iter()
                .map(|chunk| Ok(Bytes::from_static(chunk)))
                .collect();
            if self.fail {
                chunks.push(Err(io::Error::other("download failed")));
            }
            WithSize {
                size: None,
                contents: stream::iter(chunks),
            }
        }
    }

    /// Store a fake resource at a fixed recording-relative path.
    async fn store(storage: &FsStorage, source: &FakeSource) -> Result<(), RecordError> {
        storage
            .store_resource(
                source,
                &Url::parse("https://example.com/segment.ts").unwrap(),
                None,
                &NoProgress,
                RelativePath::new("segment.ts"),
                CancellationToken::new(),
            )
            .await
    }

    /// Check that a failed download left no files behind.
    async fn assert_no_files(dest: &Path) {
        assert!(!fs::try_exists(dest.join("segment.ts")).await.unwrap());
        assert!(!fs::try_exists(dest.join("segment.ts.part")).await.unwrap());
    }

    #[tokio::test]
    async fn successful_store() {
        let dest = tempfile::tempdir().unwrap();
        let storage = FsStorage::new(dest.path());
        store(&storage, &FakeSource::new(vec![b"hello", b" world"], false))
            .await
            .unwrap();
        assert_eq!(
            fs::read(dest.path().join("segment.ts")).await.unwrap(),
            b"hello world"
        );
        assert!(
            !fs::try_exists(dest.path().join("segment.ts.part"))
                .await
                .unwrap()
        );
    }

    #[tokio::test]
    async fn failed_request_can_be_retried() {
        let dest = tempfile::tempdir().unwrap();
        let storage = FsStorage::new(dest.path());
        assert!(
            store(&storage, &FakeSource::new(vec![], true))
                .await
                .is_err()
        );
        assert_no_files(dest.path()).await;
        store(&storage, &FakeSource::new(vec![b"retry"], false))
            .await
            .unwrap();
        assert_eq!(
            fs::read(dest.path().join("segment.ts")).await.unwrap(),
            b"retry"
        );
        assert!(
            !fs::try_exists(dest.path().join("segment.ts.part"))
                .await
                .unwrap()
        );
    }

    #[tokio::test]
    async fn failed_stream_removes_partial_file() {
        let dest = tempfile::tempdir().unwrap();
        let storage = FsStorage::new(dest.path());
        assert!(
            store(&storage, &FakeSource::new(vec![b"partial"], true))
                .await
                .is_err()
        );
        assert_no_files(dest.path()).await;
    }

    #[tokio::test]
    async fn existing_file_is_not_requested() {
        let dest = tempfile::tempdir().unwrap();
        fs::write(dest.path().join("segment.ts"), b"original")
            .await
            .unwrap();
        let source = FakeSource::new(vec![], true);
        store(&FsStorage::new(dest.path()), &source).await.unwrap();
        assert_eq!(source.requests.load(Ordering::SeqCst), 0);
        assert_eq!(
            fs::read(dest.path().join("segment.ts")).await.unwrap(),
            b"original"
        );
        assert!(
            !fs::try_exists(dest.path().join("segment.ts.part"))
                .await
                .unwrap()
        );
    }

    #[tokio::test]
    async fn stale_partial_file_is_overwritten() {
        let dest = tempfile::tempdir().unwrap();
        fs::write(
            dest.path().join("segment.ts.part"),
            b"stale partial contents",
        )
        .await
        .unwrap();
        store(
            &FsStorage::new(dest.path()),
            &FakeSource::new(vec![b"new"], false),
        )
        .await
        .unwrap();
        assert_eq!(
            fs::read(dest.path().join("segment.ts")).await.unwrap(),
            b"new"
        );
        assert!(
            !fs::try_exists(dest.path().join("segment.ts.part"))
                .await
                .unwrap()
        );
    }

    #[tokio::test]
    async fn empty_resource_is_stored() {
        let dest = tempfile::tempdir().unwrap();
        store(
            &FsStorage::new(dest.path()),
            &FakeSource::new(vec![], false),
        )
        .await
        .unwrap();
        assert!(
            fs::read(dest.path().join("segment.ts"))
                .await
                .unwrap()
                .is_empty()
        );
        assert!(
            !fs::try_exists(dest.path().join("segment.ts.part"))
                .await
                .unwrap()
        );
    }
}
