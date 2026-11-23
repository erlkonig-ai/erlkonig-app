//! Offscreen AppKit app icon, using the exact production resting crown.
//! No application, status item, timer, service or network is created.

#[allow(dead_code)]
#[path = "../../../ios/native/crates/erlkonig-crown/src/lib.rs"]
mod crown;

use objc2::{AnyThread, rc::Retained};
use objc2_app_kit::{
    NSBezierPath, NSBitmapImageFileType, NSBitmapImageRep, NSCalibratedRGBColorSpace, NSColor,
    NSGraphicsContext,
};
use objc2_foundation::{NSDictionary, NSPoint, NSRect, NSSize};
use std::{path::Path, time::Duration};

fn render(size: usize, points: &[crown::Point]) -> Retained<NSBitmapImageRep> {
    // AppKit owns explicit RGBA8 storage, freshly cleared to transparent.
    let bitmap = unsafe {
        NSBitmapImageRep::initWithBitmapDataPlanes_pixelsWide_pixelsHigh_bitsPerSample_samplesPerPixel_hasAlpha_isPlanar_colorSpaceName_bytesPerRow_bitsPerPixel(
            NSBitmapImageRep::alloc(), std::ptr::null_mut(), size as isize,
            size as isize, 8, 4, true, false, NSCalibratedRGBColorSpace,
            (size * 4) as isize, 32,
        )
    }.expect("RGBA bitmap");
    unsafe {
        std::slice::from_raw_parts_mut(bitmap.bitmapData(), size * size * 4).fill(0);
    }
    let context =
        NSGraphicsContext::graphicsContextWithBitmapImageRep(&bitmap).expect("offscreen context");
    NSGraphicsContext::saveGraphicsState_class();
    NSGraphicsContext::setCurrentContext(Some(&context));
    let size = size as f64;
    let inset = size / 16.0;
    let tile = NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(
        NSRect::new(
            NSPoint::new(inset, inset),
            NSSize::new(size - 2.0 * inset, size - 2.0 * inset),
        ),
        size * 0.185,
        size * 0.185,
    );
    NSColor::colorWithSRGBRed_green_blue_alpha(32.0 / 255.0, 35.0 / 255.0, 38.0 / 255.0, 1.0)
        .setFill();
    tile.fill();

    let min_x = points.iter().map(|p| p.x).fold(f64::INFINITY, f64::min);
    let max_x = points.iter().map(|p| p.x).fold(f64::NEG_INFINITY, f64::max);
    let min_y = points.iter().map(|p| p.y).fold(f64::INFINITY, f64::min);
    let max_y = points.iter().map(|p| p.y).fold(f64::NEG_INFINITY, f64::max);
    let scale = size * 0.64 / (max_x - min_x);
    let centre_x = (min_x + max_x) * 0.5;
    let centre_y = (min_y + max_y) * 0.5;
    let position = |point: &crown::Point| {
        // Production coordinates are top-down; AppKit's bitmap context is not.
        NSPoint::new(
            size * 0.5 + (point.x - centre_x) * scale,
            size * 0.5 - (point.y - centre_y) * scale,
        )
    };
    let crown = NSBezierPath::bezierPath();
    crown.moveToPoint(position(&points[0]));
    for point in &points[1..] {
        crown.lineToPoint(position(point));
    }
    crown.closePath();
    NSColor::whiteColor().setFill();
    crown.fill();
    NSGraphicsContext::restoreGraphicsState_class();
    bitmap
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let destination = std::env::args()
        .nth(1)
        .ok_or("usage: erlkonig-app-icon OUTPUT_ICONSET")?;
    let destination = Path::new(&destination);
    std::fs::create_dir_all(destination)?;
    let idle = crown::frame(Duration::ZERO, None, false, false);
    assert!(!idle.animating);
    assert_eq!(idle, crown::frame(Duration::MAX, None, false, false));
    // Render each required backing size directly from the vector; never scale
    // a raster or substitute a small-size shape for the production geometry.
    for (points, scale) in [
        (16, 1),
        (16, 2),
        (32, 1),
        (32, 2),
        (128, 1),
        (128, 2),
        (256, 1),
        (256, 2),
        (512, 1),
        (512, 2),
    ] {
        let size = points * scale;
        let bitmap = render(size, &idle.points);
        assert_eq!(
            (bitmap.pixelsWide(), bitmap.pixelsHigh()),
            (size as isize, size as isize)
        );
        assert_eq!(bitmap.bytesPerRow(), (size * 4) as isize);
        let pixels = unsafe { std::slice::from_raw_parts(bitmap.bitmapData(), size * size * 4) };
        assert!(pixels.chunks_exact(4).any(|p| p == [255, 255, 255, 255]));
        assert!(pixels.chunks_exact(4).any(|p| p[3] > 0 && p[3] < 255));
        assert!(
            (0..size)
                .all(|x| pixels[x * 4 + 3] == 0 && pixels[((size - 1) * size + x) * 4 + 3] == 0)
        );
        assert!(
            (0..size).all(
                |y| pixels[y * size * 4 + 3] == 0 && pixels[(y * size + size - 1) * 4 + 3] == 0
            )
        );
        let png = unsafe {
            bitmap.representationUsingType_properties(
                NSBitmapImageFileType::PNG,
                &NSDictionary::new(),
            )
        }
        .expect("PNG representation");
        let retina = if scale == 2 { "@2x" } else { "" };
        let name = format!("icon_{points}x{points}{retina}.png");
        std::fs::write(destination.join(&name), png.to_vec())?;
        println!(
            "{name}: {size}x{size}, exact production idle vector, white on charcoal, transparent border"
        );
    }
    Ok(())
}
