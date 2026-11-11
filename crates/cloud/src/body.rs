//! A streaming request body for one byte range of a file (a multipart part).
//!
//! `upload::body::BodyPlan` can only stream a whole file, so parts use this small
//! equivalent: 64 KiB reads, exact length, cancellation checked on every poll, progress
//! reported as `base + bytes sent in this attempt`. If the file ends early the stream fails
//! and `changed` is set so the caller reports [`crate::CloudError::SourceChanged`].

use std::io::SeekFrom;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use bytes::Bytes;
use futures_util::StreamExt as _;
use tokio::io::{AsyncReadExt as _, AsyncSeekExt as _};
use tokio_util::sync::CancellationToken;
use upload::ProgressSink;
use upload::body::CHUNK;

use crate::error::CloudError;

/// Everything a part stream needs to report into.
pub(crate) struct PartSink {
    pub progress: Arc<dyn ProgressSink>,
    pub cancel: CancellationToken,
    /// Bytes of the whole upload sent before this part.
    pub base: u64,
    /// Total file size.
    pub total: u64,
    /// Set when the file turned out shorter than planned.
    pub changed: Arc<AtomicBool>,
}

struct State {
    file: tokio::fs::File,
    remaining: u64,
    sent: u64,
    sink: PartSink,
    failed: bool,
}

/// Open `path`, seek to `offset` and stream exactly `len` bytes.
pub(crate) async fn ranged_body(
    path: &Path,
    offset: u64,
    len: u64,
    sink: PartSink,
) -> Result<reqwest::Body, CloudError> {
    let mut file = tokio::fs::File::open(path)
        .await
        .map_err(|e| CloudError::io(format!("opening {}", path.display()), e))?;
    file.seek(SeekFrom::Start(offset))
        .await
        .map_err(|e| CloudError::io("seeking in the file", e))?;
    let state = State {
        file,
        remaining: len,
        sent: 0,
        sink,
        failed: false,
    };
    let stream = futures_util::stream::unfold(state, |mut st| async move {
        let item = st.next_chunk().await?;
        Some((item, st))
    });
    Ok(reqwest::Body::wrap_stream(stream.boxed()))
}

impl State {
    async fn next_chunk(&mut self) -> Option<std::io::Result<Bytes>> {
        if self.failed || self.remaining == 0 {
            return None;
        }
        if self.sink.cancel.is_cancelled() {
            self.failed = true;
            return Some(Err(std::io::Error::new(
                std::io::ErrorKind::Interrupted,
                "cancelled",
            )));
        }
        let want = usize::try_from(self.remaining).map_or(CHUNK, |r| r.min(CHUNK));
        let mut buf = vec![0u8; want];
        let mut filled = 0;
        // Fill the chunk completely so every read the server sees is CHUNK-sized.
        while filled < want {
            match self.file.read(&mut buf[filled..]).await {
                Ok(0) => break,
                Ok(n) => filled += n,
                Err(e) => {
                    self.failed = true;
                    return Some(Err(e));
                }
            }
        }
        if filled == 0 {
            self.failed = true;
            self.sink.changed.store(true, Ordering::SeqCst);
            return Some(Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "file shrank while uploading",
            )));
        }
        buf.truncate(filled);
        self.remaining -= filled as u64;
        self.sent += filled as u64;
        self.sink
            .progress
            .report(self.sink.base + self.sent, Some(self.sink.total));
        Some(Ok(Bytes::from(buf)))
    }
}
