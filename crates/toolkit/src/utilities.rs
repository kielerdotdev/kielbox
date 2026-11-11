//! Local utility jobs. No shell, network upload, automatic overwrite or UI-thread work.
use crate::{
    core::workflow::{CancelToken, Clipboard, ClipboardContent, QrRenderer},
    effects as fx, services,
    types::{EncodeOptions, Frame, ImageFormat, Point, Rect},
};
use anyhow::{Context, Result, bail, ensure};
use sha2::{Digest, Sha256, Sha384, Sha512};
use std::{
    fmt::Write as _,
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
    time::Duration,
};

pub fn with_pool<T: Send>(work: impl FnOnce() -> T + Send) -> Result<T> {
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(2)
        .thread_name(|i| format!("rcr-utility-render-{i}"))
        .build()?;
    Ok(pool.install(work))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tool {
    Color,
    Viewer,
    Beautify,
    Effects,
    Combine,
    Split,
    Thumbnail,
    Convert,
    VideoConvert,
    VideoThumbnail,
    Ocr,
    QrEncode,
    QrDecode,
    Hash,
    Index,
    Clipboard,
    Windows,
}
impl Tool {
    pub const ALL: [Self; 17] = [
        Self::Color,
        Self::Viewer,
        Self::Beautify,
        Self::Effects,
        Self::Combine,
        Self::Split,
        Self::Thumbnail,
        Self::Convert,
        Self::VideoConvert,
        Self::VideoThumbnail,
        Self::Ocr,
        Self::QrEncode,
        Self::QrDecode,
        Self::Hash,
        Self::Index,
        Self::Clipboard,
        Self::Windows,
    ];
    pub fn name(self) -> &'static str {
        match self {
            Self::Color => "Color picker",
            Self::Viewer => "Image viewer",
            Self::Beautify => "Image beautifier",
            Self::Effects => "Image effects",
            Self::Combine => "Image combiner",
            Self::Split => "Image splitter",
            Self::Thumbnail => "Image thumbnailer",
            Self::Convert => "Image converter",
            Self::VideoConvert => "Video converter",
            Self::VideoThumbnail => "Video thumbnailer",
            Self::Ocr => "OCR",
            Self::QrEncode => "QR code",
            Self::QrDecode => "Read QR code",
            Self::Hash => "Hash checker",
            Self::Index => "Directory indexer",
            Self::Clipboard => "Clipboard viewer",
            Self::Windows => "Inspect / borderless window",
        }
    }
    pub fn description(self) -> &'static str {
        match self {
            Self::Color => "Convert a typed #RRGGBB / #RRGGBBAA color to RGB, HSL and HSV.",
            Self::Viewer => "Preview an image, copy it or save a copy.",
            Self::Beautify => {
                "Add rounded corners, a soft shadow and a colored background. Padding is in pixels."
            }
            Self::Effects => "Apply an effect to a copy. The original file stays intact.",
            Self::Combine => {
                "Combine images in the listed order, vertically or horizontally, with an optional gap."
            }
            Self::Split => {
                "Split into columns × rows, including every edge pixel. Save into a new folder under your chosen output directory."
            }
            Self::Thumbnail => {
                "Fit inside maximum width × height, preserving aspect ratio without upscaling."
            }
            Self::Convert => "Convert an image to PNG, JPEG, WebP or BMP using Save image.",
            Self::VideoConvert => {
                "Convert to MP4, WebM or GIF using installed FFmpeg. Audio is retained in MP4/WebM. Choose the output file before running."
            }
            Self::VideoThumbnail => {
                "Extract a PNG video frame at the timestamp in seconds using installed FFmpeg."
            }
            Self::Ocr => {
                "Extract text locally with Windows OCR. Requires an installed Windows OCR language pack."
            }
            Self::QrEncode => {
                "Create a QR image from text or a URL. Copy or save the generated image."
            }
            Self::QrDecode => "Read QR codes from an image locally.",
            Self::Hash => {
                "Stream files through MD5, SHA-1, SHA-256, SHA-384, SHA-512 and CRC32. Paste an expected digest to compare."
            }
            Self::Index => {
                "Generate a text, HTML, JSON or XML folder index. Depth 0 means all levels. Links and hidden entries are skipped."
            }
            Self::Clipboard => "Read the current clipboard image, text or file list on demand.",
            Self::Windows => "List visible windows and inspect their details.",
        }
    }
    pub fn accepts_files(self) -> bool {
        !matches!(
            self,
            Self::Color | Self::QrEncode | Self::Index | Self::Clipboard | Self::Windows
        )
    }
}
pub struct Request {
    pub tool: Tool,
    pub files: Vec<PathBuf>,
    pub text: String,
    pub mode: usize,
    pub a: u32,
    pub b: u32,
    pub color: String,
    pub output: Option<PathBuf>,
}
pub struct Output {
    pub text: String,
    pub image: Option<std::sync::Arc<Frame>>,
    pub preview: Option<(u32, u32, Vec<u8>)>,
    pub files: Vec<PathBuf>,
}
impl Output {
    fn text(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            image: None,
            preview: None,
            files: vec![],
        }
    }
    fn image(frame: Frame) -> Self {
        let mut out = Self::text(format!("{} × {} pixels", frame.width(), frame.height()));
        out.image = Some(std::sync::Arc::new(frame));
        out
    }
    pub fn prepare(mut self) -> Result<Self> {
        if let Some(f) = &self.image {
            let (w, h) = fit(f.width(), f.height(), 1000, 600);
            let p = fx::resize(f, w, h, fx::ResizeFilter::Bilinear)?;
            let mut bytes = p.into_data();
            for p in bytes.as_chunks_mut::<4>().0 {
                p.swap(0, 2);
            }
            self.preview = Some((w, h, bytes));
        }
        Ok(self)
    }
}
fn load(path: &Path) -> Result<Frame> {
    let metadata = path.metadata()?;
    ensure!(
        metadata.len() <= 256 * 1024 * 1024,
        "Image file exceeds 256 MiB"
    );
    let bytes = fs::read(path)?;
    let reader = image::ImageReader::new(std::io::Cursor::new(&bytes)).with_guessed_format()?;
    let (w, h) = reader.into_dimensions()?;
    dimensions(w, h)?;
    Ok(Frame::decode(&bytes)?.into_rgba8()?)
}
fn dimensions(w: u32, h: u32) -> Result<()> {
    ensure!(
        w > 0 && h > 0 && w <= 32768 && h <= 32768 && w as u64 * h as u64 <= 32_000_000,
        "Output must be nonempty and at most 32 megapixels (32768 pixels per edge)"
    );
    Ok(())
}
pub fn fit(w: u32, h: u32, mw: u32, mh: u32) -> (u32, u32) {
    let scale = (mw.max(1) as f64 / w.max(1) as f64)
        .min(mh.max(1) as f64 / h.max(1) as f64)
        .min(1.);
    (
        (w as f64 * scale).round().max(1.) as u32,
        (h as f64 * scale).round().max(1.) as u32,
    )
}
pub fn color(text: &str) -> Result<[u8; 4]> {
    let t = text.trim().trim_start_matches('#');
    ensure!(t.len() == 6 || t.len() == 8, "Enter #RRGGBB or #RRGGBBAA");
    ensure!(t.is_ascii(), "Color must contain hexadecimal digits");
    let mut c = [0, 0, 0, 255];
    for (i, p) in t.as_bytes().as_chunks::<2>().0.iter().enumerate() {
        c[i] =
            u8::from_str_radix(std::str::from_utf8(p)?, 16).context("Invalid hexadecimal color")?;
    }
    Ok(c)
}
pub fn color_text(c: [u8; 4]) -> String {
    let [r, g, b, a] = c;
    let (rf, gf, bf) = (r as f32 / 255., g as f32 / 255., b as f32 / 255.);
    let max = rf.max(gf).max(bf);
    let min = rf.min(gf).min(bf);
    let d = max - min;
    let h = if d == 0. {
        0.
    } else if max == rf {
        60. * ((gf - bf) / d).rem_euclid(6.)
    } else if max == gf {
        60. * ((bf - rf) / d + 2.)
    } else {
        60. * ((rf - gf) / d + 4.)
    };
    let l = (max + min) / 2.;
    let s = if d == 0. {
        0.
    } else {
        d / (1. - (2. * l - 1.).abs())
    };
    let v_s = if max == 0. { 0. } else { d / max };
    format!(
        "#{r:02X}{g:02X}{b:02X}\n#{r:02X}{g:02X}{b:02X}{a:02X}\nRGB: {r}, {g}, {b}\nAlpha: {a}\nHSL: {h:.1}°, {:.1}%, {:.1}%\nHSV: {h:.1}°, {:.1}%, {:.1}%",
        s * 100.,
        l * 100.,
        v_s * 100.,
        max * 100.
    )
}
pub fn run(r: Request, cancel: &CancelToken) -> Result<Output> {
    run_with_frame(r, None, cancel)
}

/// Runs with ephemeral captured pixels; no source image is written to disk.
pub fn run_with_frame(r: Request, frame: Option<Frame>, cancel: &CancelToken) -> Result<Output> {
    cancel.check()?;
    let first = || r.files.first().context("Choose an input file");
    let image = || match &frame {
        Some(frame) => Ok(frame.clone()),
        None => load(first()?),
    };
    let out = match r.tool {
        Tool::Windows => Output::text(serde_json::to_string_pretty(&crate::window_tools::list()?)?),
        Tool::Color => {
            let c = color(&r.text)?;
            let mut o = Output::image(fx::solid_frame(240, 120, c));
            o.text = color_text(c);
            o
        }
        Tool::Viewer | Tool::Convert => Output::image(image()?),
        Tool::Beautify => {
            let mut f = image()?;
            let padding = r.a.min(512);
            fx::round_corners(&mut f, r.b.min(256) as f32, [true; 4])?;
            let shadow = fx::drop_shadow(
                &f,
                &fx::ShadowParams {
                    sigma: 12.,
                    offset_y: 8,
                    ..Default::default()
                },
            )?;
            let w = shadow
                .frame
                .width()
                .checked_add(padding * 2)
                .context("Image too wide")?;
            let h = shadow
                .frame
                .height()
                .checked_add(padding * 2)
                .context("Image too tall")?;
            dimensions(w, h)?;
            let mut bg = fx::solid_frame(w, h, color(&r.color)?);
            fx::composite_over(
                &mut bg,
                &shadow.frame,
                Point::new(padding as i32, padding as i32),
                1.,
                fx::BlendMode::Normal,
            )?;
            Output::image(bg)
        }
        Tool::Effects => {
            let f = image()?;
            let effect = match r.mode {
                0 => fx::Effect::Grayscale,
                1 => fx::Effect::Sepia,
                2 => fx::Effect::Invert,
                3 => fx::Effect::GaussianBlur {
                    sigma: r.a.clamp(1, 100) as f32,
                },
                4 => fx::Effect::Pixelate {
                    block: r.a.clamp(1, 256),
                },
                5 => fx::Effect::Sharpen {
                    amount: r.a.min(100) as f32 / 10.,
                },
                6 => fx::Effect::Brightness {
                    amount: r.a.min(200) as f32 / 100. - 1.,
                },
                7 => fx::Effect::Contrast {
                    amount: r.a.min(200) as f32 / 100. - 1.,
                },
                8 => fx::Effect::Saturation {
                    amount: r.a.min(300) as f32 / 100.,
                },
                9 => fx::Effect::HueRotate {
                    degrees: r.a.min(360) as f32,
                },
                10 => fx::Effect::Gamma {
                    gamma: r.a.clamp(1, 400) as f32 / 100.,
                },
                11 => fx::Effect::Threshold {
                    level: r.a.min(255) as u8,
                },
                12 => fx::Effect::RoundedCorners {
                    radius: r.a.min(512) as f32,
                },
                13 => fx::Effect::Border {
                    width: r.a.min(512),
                    color: color(&r.color)?,
                },
                _ => fx::Effect::DropShadow {
                    params: fx::ShadowParams::default(),
                },
            };
            Output::image(effect.apply(&f, None)?.frame)
        }
        Tool::Combine => Output::image(combine(
            &r.files,
            r.mode == 1,
            r.a.min(1024),
            color(&r.color)?,
            cancel,
        )?),
        Tool::Split => {
            let f = image()?;
            let columns = r.a;
            let rows = r.b;
            ensure!(
                columns > 0
                    && rows > 0
                    && columns <= f.width()
                    && rows <= f.height()
                    && columns as u64 * rows as u64 <= 1000,
                "Grid must have nonempty tiles and at most 1000 cells"
            );
            let root = r.output.as_deref().context("Choose an output directory")?;
            ensure!(root.is_dir(), "Output directory does not exist");
            let dir = tempfile::Builder::new()
                .prefix("rcr-split-")
                .tempdir_in(root)?;
            let mut files = vec![];
            for row in 0..rows {
                for col in 0..columns {
                    cancel.check()?;
                    let x = col as u64 * f.width() as u64 / columns as u64;
                    let y = row as u64 * f.height() as u64 / rows as u64;
                    let right = (col + 1) as u64 * f.width() as u64 / columns as u64;
                    let bottom = (row + 1) as u64 * f.height() as u64 / rows as u64;
                    let tile = f.crop(Rect::new(
                        x as i32,
                        y as i32,
                        (right - x) as u32,
                        (bottom - y) as u32,
                    ))?;
                    let p = dir
                        .path()
                        .join(format!("row-{:03}-col-{:03}.png", row + 1, col + 1));
                    save_new(&p, &tile.encode(EncodeOptions::default())?)?;
                    files.push(p);
                }
            }
            cancel.check()?;
            let path = dir.keep();
            let mut o = Output::text(format!("Saved {} tiles to {}", files.len(), path.display()));
            o.files = files;
            o
        }
        Tool::Thumbnail => {
            let f = image()?;
            let (w, h) = fit(f.width(), f.height(), r.a, r.b);
            Output::image(fx::resize(&f, w, h, fx::ResizeFilter::Lanczos3)?)
        }
        Tool::VideoConvert | Tool::VideoThumbnail => video(&r, cancel)?,
        Tool::Ocr => Output::text(extract_text(&image()?, false, cancel)?),
        Tool::QrEncode => Output::image(crate::core::workflow::QrCodeRenderer.render(&r.text)?),
        Tool::QrDecode => Output::text(extract_text(&image()?, true, cancel)?),
        Tool::Hash => Output::text(hashes(&r.files, &r.text, cancel)?),
        Tool::Index => Output::text(index(first()?, r.a.min(64), r.mode, cancel)?),
        Tool::Clipboard => match services::SystemClipboard::new(Default::default()).read()? {
            ClipboardContent::Image(f) => Output::image(f.into_rgba8()?),
            ClipboardContent::Text(t) => Output::text(t),
            ClipboardContent::Files(paths) => {
                let mut o = Output::text(
                    paths
                        .iter()
                        .map(|p| p.display().to_string())
                        .collect::<Vec<_>>()
                        .join("\n"),
                );
                o.files = paths;
                o
            }
            ClipboardContent::Empty => {
                Output::text("Clipboard has no supported image, text or files")
            }
        },
    };
    cancel.check()?;
    out.prepare()
}

pub fn extract_text(f: &Frame, qr: bool, cancel: &CancelToken) -> Result<String> {
    cancel.check()?;
    if !qr {
        return ocr(f);
    }
    let rgba = image::RgbaImage::from_raw(f.width(), f.height(), f.data().to_vec())
        .context("Invalid image")?;
    let luma = image::DynamicImage::ImageRgba8(rgba).into_luma8();
    let mut prepared = rqrr::PreparedImage::prepare(luma);
    let mut texts = vec![];
    for grid in prepared.detect_grids() {
        cancel.check()?;
        if let Ok((_, text)) = grid.decode() {
            texts.push(text);
        }
    }
    ensure!(!texts.is_empty(), "No readable QR code found");
    Ok(texts.join("\n\n"))
}
fn combine(
    paths: &[PathBuf],
    horizontal: bool,
    gap: u32,
    bg: [u8; 4],
    cancel: &CancelToken,
) -> Result<Frame> {
    ensure!(
        !paths.is_empty() && paths.len() <= 100,
        "Choose between 1 and 100 images"
    );
    let mut frames = vec![];
    let (mut w, mut h) = (0u32, 0u32);
    for p in paths {
        cancel.check()?;
        let f = load(p)?;
        if horizontal {
            w = w.checked_add(f.width()).context("Output too wide")?;
            h = h.max(f.height());
        } else {
            h = h.checked_add(f.height()).context("Output too tall")?;
            w = w.max(f.width());
        }
        dimensions(w, h)?;
        ensure!(
            frames.iter().map(|f: &Frame| f.data().len()).sum::<usize>() + f.data().len()
                <= 128_000_000,
            "Combined source images exceed 128 MB"
        );
        frames.push(f);
    }
    let gaps = gap * (paths.len() as u32 - 1);
    if horizontal {
        w = w.checked_add(gaps).context("Output too wide")?;
    } else {
        h = h.checked_add(gaps).context("Output too tall")?;
    }
    dimensions(w, h)?;
    let mut out = fx::solid_frame(w, h, bg);
    let mut offset = 0;
    for f in frames {
        cancel.check()?;
        fx::composite_over(
            &mut out,
            &f,
            if horizontal {
                Point::new(offset, 0)
            } else {
                Point::new(0, offset)
            },
            1.,
            fx::BlendMode::Normal,
        )?;
        offset += if horizontal { f.width() } else { f.height() } as i32 + gap as i32;
    }
    Ok(out)
}
fn hashes(paths: &[PathBuf], expected: &str, cancel: &CancelToken) -> Result<String> {
    ensure!(!paths.is_empty(), "Choose a file to hash");
    let mut out = String::new();
    let expected = expected.trim().to_ascii_lowercase();
    ensure!(
        expected.is_empty()
            || ([8, 32, 40, 64, 96, 128].contains(&expected.len())
                && expected.bytes().all(|b| b.is_ascii_hexdigit())),
        "Expected hash must be a hexadecimal CRC32, MD5, SHA-1, SHA-256, SHA-384 or SHA-512 digest"
    );
    for path in paths {
        cancel.check()?;
        let mut file = fs::File::open(path)?;
        let (mut md5, mut sha256, mut sha512, mut crc) = (
            md5::Md5::new(),
            Sha256::new(),
            Sha512::new(),
            crc32fast::Hasher::new(),
        );
        let (mut sha1, mut sha384) = (sha1::Sha1::new(), Sha384::new());
        let mut block = vec![0; 256 * 1024];
        let mut size = 0u64;
        loop {
            cancel.check()?;
            let n = file.read(&mut block)?;
            if n == 0 {
                break;
            }
            size += n as u64;
            md5.update(&block[..n]);
            sha1.update(&block[..n]);
            sha384.update(&block[..n]);
            sha256.update(&block[..n]);
            sha512.update(&block[..n]);
            crc.update(&block[..n]);
        }
        let digests = [
            ("MD5", format!("{:x}", md5.finalize())),
            ("SHA-1", format!("{:x}", sha1.finalize())),
            ("SHA-384", format!("{:x}", sha384.finalize())),
            ("SHA-256", format!("{:x}", sha256.finalize())),
            ("SHA-512", format!("{:x}", sha512.finalize())),
            ("CRC32", format!("{:08x}", crc.finalize())),
        ];
        writeln!(out, "{} ({size} bytes)", path.display())?;
        for (name, digest) in &digests {
            writeln!(out, "{name}: {digest}")?;
        }
        if !expected.is_empty() {
            writeln!(
                out,
                "Expected hash: {}",
                if digests.iter().any(|(_, d)| d == &expected) {
                    "MATCH"
                } else {
                    "MISMATCH"
                }
            )?;
        }
        out.push('\n');
    }
    Ok(out)
}
#[derive(serde::Serialize)]
struct IndexEntry {
    path: String,
    directory: bool,
    bytes: u64,
}
fn hidden(path: &Path, metadata: &fs::Metadata) -> bool {
    if path
        .file_name()
        .is_some_and(|s| s.to_string_lossy().starts_with('.'))
    {
        return true;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if metadata.file_attributes() & 2 != 0 {
            return true;
        }
    }
    let _ = metadata;
    false
}
fn index(root: &Path, depth: u32, mode: usize, cancel: &CancelToken) -> Result<String> {
    ensure!(root.is_dir(), "Choose a directory");
    let mut entries = vec![];
    let mut pending = vec![(root.to_owned(), 0u32)];
    while let Some((dir, level)) = pending.pop() {
        cancel.check()?;
        for e in fs::read_dir(&dir)? {
            cancel.check()?;
            let e = e?;
            let m = fs::symlink_metadata(e.path())?;
            if m.file_type().is_symlink() || hidden(&e.path(), &m) {
                continue;
            }
            #[cfg(windows)]
            {
                use std::os::windows::fs::MetadataExt;
                if m.file_attributes() & 0x400 != 0 {
                    continue;
                }
            }
            ensure!(
                entries.len() < 100_000,
                "Index exceeds 100,000 entries; choose a smaller depth"
            );
            let path = e
                .path()
                .strip_prefix(root)?
                .to_string_lossy()
                .replace('\\', "/");
            entries.push(IndexEntry {
                path,
                directory: m.is_dir(),
                bytes: if m.is_file() { m.len() } else { 0 },
            });
            if m.is_dir() && (depth == 0 || level + 1 < depth) {
                ensure!(level < 128, "Directory nesting exceeds 128 levels");
                pending.push((e.path(), level + 1));
            }
        }
    }
    entries.sort_by(|a, b| a.path.cmp(&b.path));
    match mode {
        1 => {
            let mut s = String::from(
                "<!doctype html><meta charset=\"utf-8\"><title>Directory index</title><table><tr><th>Path</th><th>Bytes</th></tr>",
            );
            for e in entries {
                write!(
                    s,
                    "<tr><td>{}{}</td><td>{}</td></tr>",
                    escape(&e.path),
                    if e.directory { "/" } else { "" },
                    e.bytes
                )?;
            }
            s.push_str("</table>");
            Ok(s)
        }
        2 => Ok(serde_json::to_string_pretty(&entries)?),
        3 => {
            let mut s = String::from("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<index>\n");
            for e in entries {
                writeln!(
                    s,
                    "  <entry path=\"{}\" directory=\"{}\" bytes=\"{}\"/>",
                    escape(&e.path),
                    e.directory,
                    e.bytes
                )?;
            }
            s.push_str("</index>\n");
            Ok(s)
        }
        _ => Ok(entries
            .into_iter()
            .map(|e| {
                format!(
                    "{}{}{}",
                    e.path,
                    if e.directory { "/" } else { "" },
                    if e.directory {
                        String::new()
                    } else {
                        format!(" ({} bytes)", e.bytes)
                    }
                )
            })
            .collect::<Vec<_>>()
            .join("\n")),
    }
}
fn escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}
pub fn save_new(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path.parent().context("Output has no directory")?;
    let mut temp = tempfile::NamedTempFile::new_in(parent)?;
    temp.write_all(bytes)?;
    temp.as_file().sync_all()?;
    temp.persist_noclobber(path)
        .map_err(|e| e.error)
        .with_context(|| format!("Cannot save {}; choose a new filename", path.display()))?;
    Ok(())
}
pub fn save_image(path: &Path, f: &Frame) -> Result<()> {
    let ext = path.extension().and_then(|s| s.to_str()).unwrap_or("");
    let format = ImageFormat::from_extension(ext).context("Use .png, .jpg, .webp or .bmp")?;
    save_new(path, &f.encode(EncodeOptions::new(format))?)
}
fn video(r: &Request, cancel: &CancelToken) -> Result<Output> {
    let input = r.files.first().context("Choose a video")?.canonicalize()?;
    let thumbnail = r.tool == Tool::VideoThumbnail;
    let destination = if thumbnail {
        None
    } else {
        Some(r.output.clone().context("Choose an output file")?)
    };
    let temp = if let Some(path) = &destination {
        tempfile::Builder::new()
            .prefix(".rcr-video-")
            .tempdir_in(path.parent().context("Output has no parent")?)?
    } else {
        tempfile::Builder::new().prefix("rcr-video-").tempdir()?
    };
    let output = temp.path().join(if thumbnail {
        "frame.png".into()
    } else {
        destination
            .as_ref()
            .unwrap()
            .file_name()
            .context("Output has no filename")?
            .to_owned()
    });
    if let Some(path) = &destination {
        ensure!(
            !path.exists(),
            "Output already exists; choose a new filename"
        );
    }
    ensure!(
        !output.exists(),
        "Output already exists; choose a new filename"
    );
    let args = video_args(&input, &output, thumbnail, r.mode, r.a, r.b)?;
    let mut spec = services::command::ProcessSpec::new("ffmpeg", &args);
    spec.timeout = Duration::from_secs(3600);
    spec.capture_stderr = true;
    let result = services::command::run_process(&spec, cancel)?;
    if !result.success {
        bail!("FFmpeg failed: {}", result.stderr);
    }
    cancel.check()?;
    if thumbnail {
        Ok(Output::image(load(&output)?))
    } else {
        let destination = destination.unwrap();
        tempfile::TempPath::try_from_path(output)?
            .persist_noclobber(&destination)
            .context("Could not publish video without overwriting the destination")?;
        let mut o = Output::text(format!("Saved {}", destination.display()));
        o.files.push(destination);
        Ok(o)
    }
}
fn video_args(
    input: &Path,
    output: &Path,
    thumbnail: bool,
    mode: usize,
    a: u32,
    b: u32,
) -> Result<Vec<String>> {
    let mut args = vec![
        "-hide_banner".into(),
        "-loglevel".into(),
        "error".into(),
        "-nostdin".into(),
        "-n".into(),
        "-threads".into(),
        "2".into(),
        "-filter_threads".into(),
        "2".into(),
        "-filter_complex_threads".into(),
        "2".into(),
    ];
    if thumbnail {
        args.extend(["-ss".into(), a.to_string()]);
    }
    args.extend(["-i".into(), input.to_string_lossy().into_owned()]);
    if thumbnail {
        args.extend([
            "-frames:v".into(),
            "1".into(),
            "-vf".into(),
            format!("scale='min({},iw)':-2", b.clamp(32, 4096)),
        ]);
    } else {
        let ext = output
            .extension()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        let expected = match mode {
            0 => "mp4",
            1 => "webm",
            _ => "gif",
        };
        ensure!(ext == expected, "Output extension must be .{expected}");
        match mode{0=>args.extend(["-map","0:v:0","-map","0:a?","-c:v","libx264","-preset","veryfast","-crf","20","-pix_fmt","yuv420p","-vf","scale=trunc(iw/2)*2:trunc(ih/2)*2","-c:a","aac","-movflags","+faststart"].map(String::from)),1=>args.extend(["-map","0:v:0","-map","0:a?","-c:v","libvpx-vp9","-row-mt","1","-crf","32","-b:v","0","-c:a","libopus"].map(String::from)),_=>args.extend(["-an".into(),"-filter_complex".into(),format!("fps={},scale='min({},iw)':-1:flags=lanczos,split[a][b];[a]palettegen[p];[b][p]paletteuse",a.clamp(1,30),b.clamp(32,1920))])};
        args.extend(["-threads".into(), "2".into()]);
    }
    args.push(output.to_string_lossy().into_owned());
    Ok(args)
}
#[cfg(windows)]
fn ocr(f: &Frame) -> Result<String> {
    use windows::{
        Graphics::Imaging::{BitmapAlphaMode, BitmapPixelFormat, SoftwareBitmap},
        Media::Ocr::OcrEngine,
        Storage::Streams::DataWriter,
    };
    let engine = OcrEngine::TryCreateFromUserProfileLanguages()
        .context("Install a Windows OCR language pack")?;
    let max = OcrEngine::MaxImageDimension()?;
    let (w, h) = fit(f.width(), f.height(), max, max);
    let f = fx::resize(f, w, h, fx::ResizeFilter::Bilinear)?;
    let mut bytes = f.into_data();
    for p in bytes.as_chunks_mut::<4>().0 {
        p.swap(0, 2);
    }
    let writer = DataWriter::new()?;
    writer.WriteBytes(&bytes)?;
    let buffer = writer.DetachBuffer()?;
    let bitmap = SoftwareBitmap::CreateCopyWithAlphaFromBuffer(
        &buffer,
        BitmapPixelFormat::Bgra8,
        w as i32,
        h as i32,
        BitmapAlphaMode::Ignore,
    )?;
    let result = engine.RecognizeAsync(&bitmap)?.get()?;
    Ok(result.Text()?.to_string())
}
#[cfg(not(windows))]
fn ocr(_: &Frame) -> Result<String> {
    bail!("Local OCR is currently supported on Windows")
}

#[cfg(test)]
mod tests {
    use super::*;
    fn request(tool: Tool) -> Request {
        Request {
            tool,
            files: vec![],
            text: String::new(),
            mode: 0,
            a: 2,
            b: 2,
            color: "#123456".into(),
            output: None,
        }
    }
    #[test]
    fn captured_pixels_need_no_file_and_text_save_refuses_overwrite() {
        let cancel = CancelToken::new();
        let frame = fx::solid_frame(7, 5, [12, 34, 56, 255]);
        let output = run_with_frame(request(Tool::Viewer), Some(frame), &cancel).unwrap();
        assert_eq!(output.image.unwrap().width(), 7);
        assert!(output.files.is_empty());
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("report.txt");
        save_new(&path, b"first").unwrap();
        assert!(save_new(&path, b"second").is_err());
        assert_eq!(fs::read(path).unwrap(), b"first");
    }
    #[test]
    fn hash_vectors_compare_and_never_change_input() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("a.bin");
        fs::write(&p, b"abc").unwrap();
        let s = hashes(
            std::slice::from_ref(&p),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
            &CancelToken::new(),
        )
        .unwrap();
        assert!(
            s.contains("900150983cd24fb0d6963f7d28e17f72")
                && s.contains("352441c2")
                && s.contains("MATCH")
        );
        assert_eq!(fs::read(p).unwrap(), b"abc");
    }
    #[test]
    fn qr_round_trip_and_color_validation() {
        let mut r = request(Tool::QrEncode);
        r.text = "https://example.org/rcr?x=✓".into();
        let out = run(r, &CancelToken::new()).unwrap();
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("qr.png");
        save_image(&p, out.image.as_ref().unwrap()).unwrap();
        let mut r = request(Tool::QrDecode);
        r.files.push(p);
        assert_eq!(
            run(r, &CancelToken::new()).unwrap().text,
            "https://example.org/rcr?x=✓"
        );
        assert!(color("#１２３４５６").is_err());
        assert_eq!(color("#12345680").unwrap(), [18, 52, 86, 128]);
        assert_eq!(fit(200, 100, 50, 50), (50, 25));
    }
    #[test]
    fn split_covers_odd_edges_and_save_never_overwrites() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("input.png");
        let mut f = fx::solid_frame(7, 5, [0, 0, 0, 255]);
        for y in 0..5 {
            for (x, p) in f.row_mut(y).as_chunks_mut::<4>().0.iter_mut().enumerate() {
                p[0] = (x + y as usize * 7) as u8;
            }
        }
        save_image(&p, &f).unwrap();
        assert!(save_image(&p, &f).is_err());
        let mut r = request(Tool::Split);
        r.files.push(p);
        r.a = 3;
        r.b = 2;
        r.output = Some(d.path().to_owned());
        let out = run(r, &CancelToken::new()).unwrap();
        let mut seen = vec![];
        for p in out.files {
            let f = load(&p).unwrap();
            seen.extend(f.data().as_chunks::<4>().0.iter().map(|p| p[0]));
        }
        seen.sort();
        assert_eq!(seen, (0..35).collect::<Vec<_>>());
    }
    #[test]
    fn index_escapes_names_and_obeys_depth_and_cancel() {
        let d = tempfile::tempdir().unwrap();
        fs::create_dir(d.path().join("nested")).unwrap();
        fs::write(d.path().join("nested/a&b.txt"), b"abc").unwrap();
        fs::write(d.path().join(".hidden"), b"secret").unwrap();
        let c = CancelToken::new();
        assert!(!index(d.path(), 1, 0, &c).unwrap().contains("a&b"));
        let html = index(d.path(), 0, 1, &c).unwrap();
        assert!(html.contains("a&amp;b") && !html.contains("secret"));
        c.cancel();
        assert!(index(d.path(), 0, 0, &c).is_err());
    }
    #[test]
    fn media_arguments_preserve_tracks_and_bound_workers() {
        let args = video_args(
            Path::new("in;name.mkv"),
            Path::new("out.mp4"),
            false,
            0,
            0,
            0,
        )
        .unwrap();
        assert!(args.contains(&"0:a?".into()) && args.contains(&"-n".into()));
        assert!(args.windows(2).any(|a| a == ["-threads", "2"]));
        assert!(video_args(Path::new("in.mkv"), Path::new("out.gif"), false, 0, 0, 0).is_err());
    }
}
