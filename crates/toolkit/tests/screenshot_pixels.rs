use toolkit::types::{ColorSpace, Frame, PixelFormat, Point, Size};
use std::time::Duration;
#[test]
fn packed_rgba_conversion_reuses_pixels_and_preserves_capture_metadata() {
    for format in [PixelFormat::Bgra8, PixelFormat::Rgba8] {
        let mut frame = Frame::from_raw(
            Size::new(2, 1),
            8,
            format,
            ColorSpace::Srgb,
            vec![10, 20, 30, 40, 50, 60, 70, 80, 99],
        )
        .unwrap();
        frame.origin = Point::new(-2560, 120);
        frame.scale_factor = 1.5;
        frame.timestamp = Some(Duration::from_millis(17));
        frame.sdr_white_nits = Some(203.);
        let allocation = frame.data().as_ptr();
        let output = frame.into_rgba8().unwrap();
        assert_eq!(output.data().as_ptr(), allocation);
        assert_eq!(output.format(), PixelFormat::Rgba8);
        assert_eq!(
            output.data(),
            if format == PixelFormat::Bgra8 {
                &[30, 20, 10, 40, 70, 60, 50, 80]
            } else {
                &[10, 20, 30, 40, 50, 60, 70, 80]
            }
        );
        assert_eq!(output.origin, Point::new(-2560, 120));
        assert_eq!(output.scale_factor, 1.5);
        assert_eq!(output.timestamp, Some(Duration::from_millis(17)));
        assert_eq!(output.sdr_white_nits, Some(203.));
    }
}

#[test]
fn padded_rgba_conversion_removes_padding_and_keeps_alpha() {
    for format in [PixelFormat::Bgra8, PixelFormat::Rgba8] {
        let frame = Frame::from_raw(
            Size::new(1, 2),
            8,
            format,
            ColorSpace::Srgb,
            vec![10, 20, 30, 40, 99, 99, 99, 99, 50, 60, 70, 80],
        )
        .unwrap();
        let output = frame.into_rgba8().unwrap();
        assert_eq!(output.stride(), 4);
        assert_eq!(
            output.data(),
            if format == PixelFormat::Bgra8 {
                &[30, 20, 10, 40, 70, 60, 50, 80]
            } else {
                &[10, 20, 30, 40, 50, 60, 70, 80]
            }
        );
    }
}
