//! Deterministic offscreen AppKit proof, using the production vector renderer.
//! cargo run --release -p erlkonig --example crown-preview -- OUTPUT_DIRECTORY
//! Does not create an application, window, status item, timer or network service.

#[allow(dead_code)]
#[path = "../crates/erlkonig/src/crown.rs"]
mod crown;

use objc2::{AnyThread, rc::Retained};
use objc2_app_kit::{
    NSBitmapImageFileType, NSBitmapImageRep, NSCalibratedRGBColorSpace,
    NSCustomImageRep, NSGraphicsContext, NSImage,
};
use objc2_foundation::{NSDictionary, NSPoint, NSRect, NSSize};
use std::{fmt::Write as _, path::Path, time::Duration};

fn render(image: &NSImage, scale: usize) -> Retained<NSBitmapImageRep> {
    let width = crown::WIDTH as usize * scale;
    let height = crown::HEIGHT as usize * scale;
    // Null planes ask AppKit to own the storage. Explicit RGBA8 row geometry
    // makes the subsequent bounded pixel inspection unambiguous.
    let bitmap = unsafe {
        NSBitmapImageRep::initWithBitmapDataPlanes_pixelsWide_pixelsHigh_bitsPerSample_samplesPerPixel_hasAlpha_isPlanar_colorSpaceName_bytesPerRow_bitsPerPixel(
            NSBitmapImageRep::alloc(), std::ptr::null_mut(), width as isize,
            height as isize, 8, 4, true, false, NSCalibratedRGBColorSpace,
            (width * 4) as isize, 32,
        )
    }.expect("RGBA bitmap");
    unsafe {
        std::slice::from_raw_parts_mut(bitmap.bitmapData(), width * height * 4).fill(0);
    }
    let context = NSGraphicsContext::graphicsContextWithBitmapImageRep(&bitmap)
        .expect("offscreen context");
    NSGraphicsContext::saveGraphicsState_class();
    NSGraphicsContext::setCurrentContext(Some(&context));
    image.drawInRect(NSRect::new(NSPoint::ZERO, NSSize::new(width as f64, height as f64)));
    NSGraphicsContext::restoreGraphicsState_class();
    bitmap
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let destination = std::env::args().nth(1).ok_or("usage: crown-preview OUTPUT_DIRECTORY")?;
    if destination == "--compare" {
        return compare(Path::new(&std::env::args().nth(2).ok_or("usage: crown-preview --compare OUTPUT_DIRECTORY")?));
    }
    let destination = Path::new(&destination);
    std::fs::create_dir_all(destination)?;
    let cases = [
        ("idle", 0, None, false),
        ("attack", 80, Some(80), false),
        ("transfer", 1000, Some(1000), false),
        ("settled", 1600, Some(1000), false),
        ("reduced", 1000, Some(1000), true),
    ];
    let mut svg = String::from("<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"1100\" height=\"380\"><rect width=\"1100\" height=\"380\" fill=\"white\"/><style>text{font:14px system-ui,sans-serif}</style><text x=\"20\" y=\"25\">Production AppKit vector · 24 pt silhouette in 26 × 18 pt image · 2 pt motion bound</text>");
    let mut idle_pixels = Vec::new();
    for (column, (label, now, last, reduced)) in cases.iter().enumerate() {
        let activity = last.map(|last| crown::TransferActivity {
            began_at: Duration::ZERO,
            last_transfer_at: Duration::from_millis(last),
        });
        let image = crown::frame(Duration::from_millis(*now), activity, *reduced).template_image();
        assert!(image.isTemplate());
        assert_eq!(image.size(), NSSize::new(crown::WIDTH, crown::HEIGHT));
        let reps = image.representations();
        assert_eq!(reps.len(), 1);
        assert!(reps.objectAtIndex(0).downcast_ref::<NSCustomImageRep>().is_some());
        let x = 20 + column * 215;
        writeln!(svg, "<text x=\"{x}\" y=\"55\">{label} · template=true</text>")?;
        for scale in [1, 2] {
            let bitmap = render(&image, scale);
            let width = bitmap.pixelsWide() as usize;
            let height = bitmap.pixelsHigh() as usize;
            assert_eq!((width, height), (26 * scale, 18 * scale));
            assert_eq!(bitmap.bytesPerRow() as usize, width * 4);
            let pixels = unsafe {
                std::slice::from_raw_parts(bitmap.bitmapData(), width * height * 4)
            };
            assert!(pixels.chunks_exact(4).all(|p| p[..3] == [0, 0, 0]));
            let solid = pixels.chunks_exact(4).filter(|p| p[3] == 255).count();
            let partial = pixels.chunks_exact(4).filter(|p| p[3] > 0 && p[3] < 255).count();
            assert!(solid > 0 && partial > 0);
            for row in 0..height {
                assert_eq!(pixels[(row * width) * 4 + 3], 0);
                assert_eq!(pixels[(row * width + width - 1) * 4 + 3], 0);
            }
            assert!(pixels[..width * 4].iter().all(|p| *p == 0));
            assert!(pixels[(height - 1) * width * 4..].iter().all(|p| *p == 0));
            if scale == 2 {
                if *label == "idle" { idle_pixels = pixels.to_vec(); }
                if *label == "settled" || *label == "reduced" { assert_eq!(pixels, idle_pixels); }
                if *label == "transfer" { assert_ne!(pixels, idle_pixels); }
            }
            // Empty property dictionary carries no untyped option values.
            let png = unsafe {
                bitmap.representationUsingType_properties(
                    NSBitmapImageFileType::PNG, &NSDictionary::new(),
                )
            }.expect("PNG representation");
            std::fs::write(destination.join(format!("{label}-{scale}x.png")), png.to_vec())?;
            println!("{label} {scale}x: {width}×{height}, {solid} opaque pixels, {partial} antialiased pixels, custom vector rep, template=true");
            let top = if scale == 1 { 70 } else { 220 };
            let zoom = if scale == 1 { 7 } else { 3 };
            writeln!(svg, "<g transform=\"translate({x} {top}) scale({zoom})\" fill=\"black\">")?;
            for (i, pixel) in pixels.chunks_exact(4).enumerate().filter(|(_, p)| p[3] != 0) {
                writeln!(svg, "<rect x=\"{}\" y=\"{}\" width=\"1\" height=\"1\" opacity=\"{:.4}\"/>", i % width, i / width, f64::from(pixel[3]) / 255.0)?;
            }
            writeln!(svg, "</g><text x=\"{x}\" y=\"{}\">Native {scale}× alpha mask, zoom {zoom}×</text>", top + height * zoom + 20)?;
        }
    }
    svg.push_str("</svg>\n");
    std::fs::write(destination.join("contact-sheet.svg"), svg)?;
    Ok(())
}

/// Compare previous and current native 2x alpha masks. Reconstruct the previous
/// strength from the production frame's inner-wave deltas; all fixed silhouette
/// points stay fixed. This does not change the renderer or the running app.
fn compare(destination: &Path) -> Result<(), Box<dyn std::error::Error>> {
    std::fs::create_dir_all(destination)?;
    let idle = crown::frame(Duration::ZERO, None, false);
    let idle_bitmap = render(&idle.template_image(), 2);
    let idle_pixels = unsafe { std::slice::from_raw_parts(idle_bitmap.bitmapData(), 52 * 36 * 4) };
    let phases = [160, 1000, 2500, 4000];
    let zoom = 4;
    let width = 900;
    let height = 390;
    let sheet = unsafe {
        NSBitmapImageRep::initWithBitmapDataPlanes_pixelsWide_pixelsHigh_bitsPerSample_samplesPerPixel_hasAlpha_isPlanar_colorSpaceName_bytesPerRow_bitsPerPixel(
            NSBitmapImageRep::alloc(), std::ptr::null_mut(), width, height,
            8, 4, true, false, NSCalibratedRGBColorSpace, width * 4, 32,
        )
    }.expect("contact sheet");
    let sheet_pixels = unsafe { std::slice::from_raw_parts_mut(sheet.bitmapData(), width as usize * height as usize * 4) };
    sheet_pixels.fill(255);
    for (column, millis) in phases.into_iter().enumerate() {
        let now = Duration::from_millis(millis);
        let current = crown::frame(now, Some(crown::TransferActivity { began_at: Duration::ZERO, last_transfer_at: now }), false);
        let mut previous = current.clone();
        for (point, rest) in previous.points.iter_mut().zip(&idle.points) {
            point.y = rest.y + (point.y - rest.y) * ((0.85 * 22.0 / 36.0) / 2.0);
        }
        for (row, (label, frame)) in [("previous-0.519pt", previous), ("current-2pt", current)].into_iter().enumerate() {
            let max_displacement = frame.points.iter().zip(&idle.points).map(|(p, r)| (p.y - r.y).abs()).fold(0f64, f64::max);
            assert!(max_displacement <= 2.000001);
            let image = frame.template_image();
            assert!(image.isTemplate());
            let bitmap = render(&image, 2);
            let pixels = unsafe { std::slice::from_raw_parts(bitmap.bitmapData(), 52 * 36 * 4) };
            assert!((0..52).all(|x| pixels[x * 4 + 3] == 0 && pixels[((35 * 52 + x) * 4) + 3] == 0));
            assert!((0..36).all(|y| pixels[(y * 52) * 4 + 3] == 0 && pixels[(y * 52 + 51) * 4 + 3] == 0));
            let mut changed = 0;
            let mut significant = 0;
            let mut alpha_delta = 0u64;
            for (p, base) in pixels.chunks_exact(4).zip(idle_pixels.chunks_exact(4)) {
                let delta = p[3].abs_diff(base[3]);
                changed += usize::from(delta > 0);
                significant += usize::from(delta >= 32);
                alpha_delta += u64::from(delta);
            }
            println!("{label} {millis}ms: native2x=52x36 max_displacement_pt={max_displacement:.4} changed_pixels={changed} delta_at_least_32={significant} alpha_area_delta_px={:.3}", alpha_delta as f64 / 255.0);
            let png = unsafe { bitmap.representationUsingType_properties(NSBitmapImageFileType::PNG, &NSDictionary::new()) }.unwrap();
            std::fs::write(destination.join(format!("{label}-{millis}ms-2x.png")), png.to_vec())?;
            // Large rows: 4x nearest-neighbour enlargement of the actual 2x mask.
            // Small rows underneath: the unchanged native 2x pixels at 1:1.
            for (scale, top) in [(zoom, row * 190 + 5), (1, row * 190 + 150)] {
                let left = column * 220 + 10;
                for y in 0..36 {
                    for x in 0..52 {
                        let grey = 255 - pixels[(y * 52 + x) * 4 + 3];
                        for dy in 0..scale {
                            for dx in 0..scale {
                                let index = ((top + y * scale + dy) * width as usize + left + x * scale + dx) * 4;
                                sheet_pixels[index..index + 3].fill(grey);
                            }
                        }
                    }
                }
            }
        }
    }
    let png = unsafe { sheet.representationUsingType_properties(NSBitmapImageFileType::PNG, &NSDictionary::new()) }.unwrap();
    std::fs::write(destination.join("comparison.png"), png.to_vec())?;
    println!("comparison.png: TOP=previous 0.5194pt bound, BOTTOM=current 2pt bound; columns=160/1000/2500/4000ms; each row includes enlarged4x and native2x1:1 masks. Native border-alpha/no-clipping checks passed. No screen capture.");
    Ok(())
}
