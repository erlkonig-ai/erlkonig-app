//! The crown as an AppKit template image, drawn from the shared vector.

use erlkonig_app::crown::{Frame, HEIGHT, WIDTH};

pub trait TemplateImage {
    fn template_image(&self) -> objc2::rc::Retained<objc2_app_kit::NSImage>;
}

impl TemplateImage for Frame {
    /// AppKit redraws this vector at the destination backing scale. There is
    /// no intermediate PNG, fixed-resolution bitmap, resampling or fake stroke.
    fn template_image(&self) -> objc2::rc::Retained<objc2_app_kit::NSImage> {
        use block2::RcBlock;
        use objc2::runtime::Bool;
        use objc2_app_kit::{NSBezierPath, NSColor, NSImage};
        use objc2_foundation::{NSPoint, NSRect, NSSize};

        // The block owns only immutable Rust points; AppKit may invoke it on
        // any drawing thread, so all AppKit drawing objects are local to it.
        let points = self.points.clone();
        let draw = RcBlock::new(move |_rect: NSRect| {
            let path = NSBezierPath::bezierPath();
            path.moveToPoint(NSPoint::new(points[0].x, points[0].y));
            for point in &points[1..] {
                path.lineToPoint(NSPoint::new(point.x, point.y));
            }
            path.closePath();
            NSColor::blackColor().setFill(); // fully opaque mask, never grey
            path.fill();
            Bool::YES
        });
        let image =
            NSImage::imageWithSize_flipped_drawingHandler(NSSize::new(WIDTH, HEIGHT), true, &draw);
        // Set this on EVERY frame, including transfer animation. AppKit owns
        // the tint for dark/light menu bars, selection and accessibility.
        image.setTemplate(true);
        image
    }
}
