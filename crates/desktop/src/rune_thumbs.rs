//! Library thumbnails: one background worker decodes a still per capture (a frame 20% in for
//! videos), caches it on disk keyed by path, size and modification time, and hands the view
//! ready `rune::Pixels`. Views only read; nothing here decodes on the UI thread.
use crate::recordings;
use sha2::{Digest, Sha256};
use std::{
    collections::{HashMap, VecDeque},
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::{Arc, Condvar, Mutex, mpsc},
    time::UNIX_EPOCH,
};

const MAGIC: &[u8; 8] = b"KBTHMB01";
const WIDTH: u32 = 320;
const HEIGHT: u32 = 180;
const MAX_BYTES: usize = 24 + WIDTH as usize * HEIGHT as usize * 4;
const CACHE_BUDGET: u64 = 48 * 1024 * 1024;
const IMAGE_DECODE_BUDGET: u64 = 256 * 1024 * 1024;

pub struct Thumb {
    pub pixels: rune::Pixels,
    /// Seconds, for videos.
    pub duration: Option<f64>,
    pub ready: bool,
    /// Decoded or failed: the worker is done with it.
    settled: bool,
}

struct Queue {
    paths: VecDeque<(PathBuf, rune::Pixels)>,
    stop: bool,
}

struct Done {
    path: PathBuf,
    duration: Option<f64>,
    ok: bool,
}

#[derive(Default)]
pub struct Thumbs {
    pub map: HashMap<PathBuf, Thumb>,
    queue: Option<Arc<(Mutex<Queue>, Condvar)>>,
    results: Option<mpsc::Receiver<Done>>,
    worker: Option<std::thread::JoinHandle<()>>,
}

impl Drop for Thumbs {
    fn drop(&mut self) {
        if let Some(queue) = &self.queue {
            queue.0.lock().unwrap_or_else(|e| e.into_inner()).stop = true;
            queue.1.notify_all();
        }
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

impl Thumbs {
    fn start(&mut self, wake: rune::Waker) {
        let queue = Arc::new((
            Mutex::new(Queue {
                paths: VecDeque::new(),
                stop: false,
            }),
            Condvar::new(),
        ));
        let (tx, rx) = mpsc::channel();
        let shared = queue.clone();
        let worker = std::thread::Builder::new()
            .name("kielbox-thumbnails".into())
            .spawn(move || work(shared, tx, wake));
        match worker {
            Ok(worker) => {
                self.worker = Some(worker);
                self.queue = Some(queue);
                self.results = Some(rx);
            }
            Err(e) => eprintln!("Library thumbnails unavailable: {e}"),
        }
    }

    /// Queue every listed capture without a thumbnail, in list order (newest first), and
    /// forget captures that left the list. Cheap when nothing changed.
    pub fn sync(&mut self, files: &[PathBuf], wake: &rune::Waker) {
        if self.queue.is_none() {
            self.start(wake.clone());
        }
        let Some(queue) = &self.queue else {
            return;
        };
        let listed: std::collections::HashSet<&PathBuf> = files.iter().take(500).collect();
        self.map.retain(|path, _| listed.contains(path));
        let mut wanted = VecDeque::new();
        for path in files.iter().take(500) {
            if !self.map.contains_key(path) {
                let pixels = rune::Pixels::new();
                self.map.insert(
                    path.clone(),
                    Thumb {
                        pixels: pixels.clone(),
                        duration: None,
                        ready: false,
                        settled: false,
                    },
                );
                wanted.push_back((path.clone(), pixels));
            }
        }
        if wanted.is_empty() {
            return;
        }
        let mut q = queue.0.lock().unwrap_or_else(|e| e.into_inner());
        q.paths.retain(|(p, _)| listed.contains(p));
        q.paths.extend(wanted);
        drop(q);
        queue.1.notify_all();
    }

    pub fn get(&self, path: &Path) -> Option<&Thumb> {
        self.map.get(path).filter(|t| t.ready)
    }

    /// Block until every queued thumbnail is decoded or failed, for scripted fixtures.
    pub fn wait(&mut self, timeout: std::time::Duration) {
        let start = std::time::Instant::now();
        while self.map.values().any(|t| !t.settled) && start.elapsed() < timeout {
            self.poll();
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
    }
    pub fn poll(&mut self) -> bool {
        let Some(results) = &self.results else {
            return false;
        };
        let mut changed = false;
        for done in results.try_iter() {
            if let Some(thumb) = self.map.get_mut(&done.path) {
                thumb.ready = done.ok;
                thumb.settled = true;
                thumb.duration = done.duration;
                changed = true;
            }
        }
        changed
    }
}

/// Decodes queued captures in order, filling each one's pixel buffer, until stopped.
fn work(queue: Arc<(Mutex<Queue>, Condvar)>, tx: mpsc::Sender<Done>, wake: rune::Waker) {
    loop {
        let (path, pixels) = {
            let mut q = queue.0.lock().unwrap_or_else(|e| e.into_inner());
            loop {
                if q.stop {
                    return;
                }
                if let Some(next) = q.paths.pop_front() {
                    break next;
                }
                q = queue.1.wait(q).unwrap_or_else(|e| e.into_inner());
            }
        };
        let stopped = || queue.0.lock().map(|q| q.stop).unwrap_or(true);
        let (ok, duration) = match load(&path, &stopped) {
            Some(((w, h, rgba), duration)) => {
                pixels.set(w, h, rgba);
                (true, duration)
            }
            None => (false, None),
        };
        if tx.send(Done { path, duration, ok }).is_err() {
            return;
        }
        wake();
    }
}

type Image = (u32, u32, Vec<u8>);

fn load(path: &Path, stopped: &dyn Fn() -> bool) -> Option<(Image, Option<f64>)> {
    let cache = cache_path(path);
    if let Some(hit) = cache.as_deref().and_then(read_cache) {
        return Some(hit);
    }
    let (image, duration) = if recordings::is_image(path) {
        (decode_image(path)?, None)
    } else {
        let asset = editor_core::probe_cancellable(path, stopped).ok()?;
        let duration = asset.duration_ns as f64 / 1e9;
        let frame =
            editor_core::thumbnail_cancellable(&asset, 0.2, (WIDTH, HEIGHT), stopped).ok()?;
        let mut rgba = frame.bgra;
        for p in rgba.chunks_exact_mut(4) {
            p.swap(0, 2);
            p[3] = 255;
        }
        ((frame.width, frame.height, rgba), Some(duration))
    };
    if let Some(cache) = &cache {
        write_cache(cache, &image, duration);
    }
    Some((image, duration))
}

fn decode_image(path: &Path) -> Option<Image> {
    use image::ImageDecoder;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(32_768);
    limits.max_image_height = Some(32_768);
    limits.max_alloc = Some(IMAGE_DECODE_BUDGET);
    let mut reader = image::ImageReader::open(path)
        .ok()?
        .with_guessed_format()
        .ok()?;
    reader.limits(limits);
    let decoder = reader.into_decoder().ok()?;
    if decoder.total_bytes() > IMAGE_DECODE_BUDGET {
        return None;
    }
    let image = image::DynamicImage::from_decoder(decoder).ok()?;
    let thumb = image.thumbnail(WIDTH, HEIGHT).to_rgba8();
    Some((thumb.width(), thumb.height(), thumb.into_raw()))
}

fn cache_dir() -> PathBuf {
    std::env::temp_dir().join("kielbox-thumbnails-v1")
}

fn cache_path(source: &Path) -> Option<PathBuf> {
    let metadata = std::fs::metadata(source).ok()?;
    let modified = metadata.modified().ok()?.duration_since(UNIX_EPOCH).ok()?;
    let mut digest = Sha256::new();
    digest.update(MAGIC);
    digest.update(source.as_os_str().as_encoded_bytes());
    digest.update(metadata.len().to_le_bytes());
    digest.update(modified.as_nanos().to_le_bytes());
    Some(cache_dir().join(format!("{:x}.thumb", digest.finalize())))
}

fn read_cache(path: &Path) -> Option<(Image, Option<f64>)> {
    let mut bytes = Vec::new();
    std::fs::File::open(path)
        .ok()?
        .take(MAX_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    if bytes.len() < 24 || &bytes[..8] != MAGIC {
        return None;
    }
    let w = u32::from_le_bytes(bytes[8..12].try_into().ok()?);
    let h = u32::from_le_bytes(bytes[12..16].try_into().ok()?);
    let duration = f64::from_le_bytes(bytes[16..24].try_into().ok()?);
    if !(1..=WIDTH).contains(&w)
        || !(1..=HEIGHT).contains(&h)
        || bytes.len() != 24 + w as usize * h as usize * 4
    {
        return None;
    }
    let duration = (duration.is_finite() && duration >= 0.0).then_some(duration);
    Some(((w, h, bytes[24..].to_vec()), duration))
}

fn write_cache(path: &Path, (w, h, rgba): &Image, duration: Option<f64>) {
    let Some(folder) = path.parent() else {
        return;
    };
    let write = || -> std::io::Result<()> {
        std::fs::create_dir_all(folder)?;
        let mut file = tempfile::NamedTempFile::new_in(folder)?;
        file.write_all(MAGIC)?;
        file.write_all(&w.to_le_bytes())?;
        file.write_all(&h.to_le_bytes())?;
        file.write_all(&duration.unwrap_or(-1.0).to_le_bytes())?;
        file.write_all(rgba)?;
        file.persist(path).map_err(|e| e.error)?;
        Ok(())
    };
    if write().is_ok() {
        prune(folder);
    }
}

/// Oldest cache files go first once the folder passes its budget.
fn prune(folder: &Path) {
    let Ok(entries) = std::fs::read_dir(folder) else {
        return;
    };
    let mut files: Vec<_> = entries
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x == "thumb"))
        .filter_map(|e| {
            let m = e.metadata().ok()?;
            Some((m.modified().ok(), m.len(), e.path()))
        })
        .collect();
    let mut total: u64 = files.iter().map(|f| f.1).sum();
    if total <= CACHE_BUDGET {
        return;
    }
    files.sort_by_key(|f| f.0);
    for (_, len, path) in files {
        if total <= CACHE_BUDGET {
            break;
        }
        if std::fs::remove_file(path).is_ok() {
            total = total.saturating_sub(len);
        }
    }
}
