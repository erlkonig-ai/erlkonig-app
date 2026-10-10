//! Deterministic offscreen AppKit proof, using the production vector renderer.
//! cargo run --release -p erlkonig --example crown-render -- OUTPUT_DIRECTORY
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
    let destination = std::env::args().nth(1).ok_or("usage: crown-render OUTPUT_DIRECTORY")?;
    if destination == "--ripple-phases" {
        return ripple_phases(Path::new(&std::env::args().nth(2).ok_or("usage: crown-render --ripple-phases OUTPUT_DIRECTORY")?));
    }
    let destination = Path::new(&destination);
    std::fs::create_dir_all(destination)?;
    let cases = [
        ("idle", 0, None, false),
        ("attack", 80, Some(80), false),
        ("working", 1000, Some(1000), false),
        ("landing", 3599, Some(1000), false),
        ("settled", 3600, Some(1000), false),
        ("reduced", 1000, Some(1000), true),
    ];
    let mut svg = String::from("<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"1320\" height=\"380\"><rect width=\"1320\" height=\"380\" fill=\"white\"/><style>text{font:14px system-ui,sans-serif}</style><text x=\"20\" y=\"25\">Production AppKit vector · 24 pt silhouette in 26 × 18 pt image · fixed original crest/floor envelope</text>");
    let mut idle_pixels = Vec::new();
    for (column, (label, now, last, reduced)) in cases.iter().enumerate() {
        let activity = last.map(|last| crown::WorkActivity {
            began_at: Duration::ZERO,
            ended_at: Some(Duration::from_millis(last)),
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
                if *label == "working" { assert_ne!(pixels, idle_pixels); }
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

/// Native proof for the selected travelling ripple; sampled at full attack.
fn ripple_phases(destination: &Path) -> Result<(), Box<dyn std::error::Error>> {
    std::fs::create_dir_all(destination)?;
    for (phase, millis) in [(0, 3600), (90, 900), (135, 1350), (180, 1800), (270, 2700)] {
        let now = Duration::from_millis(millis);
        let frame = crown::frame(now, Some(crown::WorkActivity {
            began_at: Duration::ZERO, ended_at: None,
        }), false);
        let image = frame.template_image();
        assert!(image.isTemplate());
        for scale in [1, 2] {
            let bitmap = render(&image, scale);
            let (width, height) = (26 * scale, 18 * scale);
            let pixels = unsafe { std::slice::from_raw_parts(bitmap.bitmapData(), width * height * 4) };
            assert!(pixels.chunks_exact(4).all(|p| p[..3] == [0, 0, 0]));
            assert!((0..width).all(|x| pixels[x * 4 + 3] == 0 && pixels[((height - 1) * width + x) * 4 + 3] == 0));
            assert!((0..height).all(|y| pixels[y * width * 4 + 3] == 0 && pixels[(y * width + width - 1) * 4 + 3] == 0));
            let png = unsafe { bitmap.representationUsingType_properties(NSBitmapImageFileType::PNG, &NSDictionary::new()) }.unwrap();
            std::fs::write(destination.join(format!("outward-ripple-phase{phase}-{scale}x.png")), png.to_vec())?;
            println!("ripple phase{phase} {scale}x: native vector, template=true, opaque black mask, border alpha=0");
        }
    }
    Ok(())
}
