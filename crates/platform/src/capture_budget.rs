//! Preflight tightly packed CPU capture/conversion/compositing buffers.
use image_types::{Frame, Monitor, PixelFormat, Rect};
use screenshot::{CaptureError, MAX_CAPTURE_BYTES, Result, validate_capture_layout};

struct Layout { native: u64, sdr: u64, allocate_sdr: bool }

fn exhausted(bytes: u64) -> CaptureError {
    CaptureError::backend("capture-memory", format!("Capture buffers require {bytes} bytes, exceeding the {MAX_CAPTURE_BYTES}-byte memory limit; capture one display or a smaller region"))
}
fn add(a: u64, b: u64) -> Result<u64> { a.checked_add(b).ok_or_else(|| exhausted(u64::MAX)) }

fn check(layouts: &[Layout], canvas: u64) -> Result<()> {
    let mut native = layouts.iter().try_fold(0, |sum, layout| add(sum, layout.native))?;
    let mut sdr = 0;
    let mut peak = native;
    for layout in layouts {
        // HDR conversion allocates its destination while the current native
        // frame, remaining batch and previously converted frames still exist.
        if layout.allocate_sdr { peak = peak.max(add(add(native, sdr)?, layout.sdr)?); }
        native -= layout.native;
        sdr = add(sdr, layout.sdr)?;
        peak = peak.max(add(native, sdr)?);
    }
    peak = peak.max(add(sdr, canvas)?);
    if peak > MAX_CAPTURE_BYTES { return Err(exhausted(peak)); }
    Ok(())
}
fn canvas_bytes(rects: impl IntoIterator<Item = Rect>, composite: bool) -> Result<u64> {
    let Some(bounds) = Rect::bounding(rects) else { return Ok(0); };
    let bytes = validate_capture_layout(bounds.size(), PixelFormat::Rgba8)?;
    Ok(if composite { bytes } else { 0 })
}

pub(super) fn monitors(monitors: &[Monitor], hdr_float: bool, composite: bool) -> Result<()> {
    let canvas = canvas_bytes(monitors.iter().map(|monitor| monitor.rect), composite)?;
    let layouts: Result<Vec<_>> = monitors.iter().map(|monitor| {
        if monitor.rect.is_empty() { return Err(CaptureError::InvalidRegion(monitor.rect)); }
        let hdr = hdr_float && monitor.hdr.is_some_and(|hdr| hdr.active);
        let native = validate_capture_layout(monitor.rect.size(), if hdr { PixelFormat::Rgba16F } else { PixelFormat::Bgra8 })?;
        let sdr = validate_capture_layout(monitor.rect.size(), PixelFormat::Rgba8)?;
        Ok(Layout { native, sdr, allocate_sdr: hdr })
    }).collect();
    check(&layouts?, canvas)
}

pub(super) fn frames(frames: &[Frame], composite: bool) -> Result<()> {
    let canvas = canvas_bytes(frames.iter().map(Frame::rect), composite)?;
    let layouts: Result<Vec<_>> = frames.iter().map(|frame| {
        validate_capture_layout(frame.size(), frame.format())?;
        let sdr = validate_capture_layout(frame.size(), PixelFormat::Rgba8)?;
        let native = frame.data().len() as u64;
        let allocate_sdr = !frame.is_sdr8() || frame.stride() != frame.width() as usize * frame.format().bytes_per_pixel();
        // In-place conversion truncates logical trailing bytes but retains the
        // allocation; do not pretend that storage is released before compositing.
        Ok(Layout { native, sdr: if allocate_sdr { sdr } else { native }, allocate_sdr })
    }).collect();
    check(&layouts?, canvas)
}

#[cfg(test)]
mod tests {
    use super::{Layout, check};
    use screenshot::MAX_CAPTURE_BYTES;

    #[test]
    fn conversion_budget_counts_hdr_destination_but_reuses_packed_sdr_storage() {
        assert!(check(&[Layout { native: MAX_CAPTURE_BYTES, sdr: MAX_CAPTURE_BYTES, allocate_sdr: false }], 0).is_ok());
        assert!(check(&[Layout { native: MAX_CAPTURE_BYTES, sdr: MAX_CAPTURE_BYTES / 2, allocate_sdr: true }], 0).is_err());
        assert!(check(&[Layout { native: MAX_CAPTURE_BYTES / 2, sdr: MAX_CAPTURE_BYTES / 2, allocate_sdr: false }], MAX_CAPTURE_BYTES / 2).is_ok());
        assert!(check(&[Layout { native: u64::MAX, sdr: 1, allocate_sdr: true }], 0).is_err());
        assert!(check(&[Layout { native: u64::MAX, sdr: 0, allocate_sdr: false }, Layout { native: 1, sdr: 0, allocate_sdr: false }], 0).is_err());
    }
}
