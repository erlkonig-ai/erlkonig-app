use crate::storage::Composition;
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send, rc::Retained};
use objc2_core_foundation::{CGPoint, CGRect, CGSize};
use objc2_foundation::{NSObjectProtocol, NSString};
use objc2_ui_kit::*;
use std::{cell::RefCell, time::Instant};

pub(super) fn rect(x: f64, y: f64, w: f64, h: f64) -> CGRect {
    CGRect::new(CGPoint::new(x, y), CGSize::new(w.max(0.), h.max(0.)))
}
pub(super) fn rgb(value: u32) -> Retained<UIColor> {
    UIColor::colorWithRed_green_blue_alpha(
        ((value >> 16) & 255) as f64 / 255.,
        ((value >> 8) & 255) as f64 / 255.,
        (value & 255) as f64 / 255.,
        1.,
    )
}
pub(super) fn collection_color(root: &str) -> u32 {
    const PALETTE: [u32; 24] = [
        0xf9a800, 0xff9b00, 0xd4652f, 0xe75b12, 0xd47479, 0xc1121c, 0x701f29, 0x904684, 0x83639d,
        0x154889, 0x2874b2, 0x07737a, 0x6a93b0, 0x48a43f, 0x81c0bb, 0x0f8558, 0x478a84, 0x468641,
        0xddaf27, 0xdb6a50, 0xe1a6ad, 0xa38995, 0x3481b8, 0xb7d9b1,
    ];
    let Ok(bytes) = hex::decode(root) else {
        return 0x888888;
    };
    if bytes.len() != 32 {
        return 0x888888;
    }
    let hash = bytes.iter().fold(1469598103934665603u64, |h, b| {
        (h ^ u64::from(*b)).wrapping_mul(1099511628211)
    });
    PALETTE[(hash % 24) as usize]
}

pub(super) fn other_used(total: u64, free: u64, c: &Composition) -> Option<u64> {
    if total == 0 || c.error.is_some() || c.known_inventories == 0 {
        return None;
    }
    let used = total.checked_sub(free)?;
    let sum = c
        .segments
        .iter()
        .try_fold(c.shared_bytes, |sum, seg| sum.checked_add(seg.bytes))?;
    (sum == c.known_bytes)
        .then(|| used.checked_sub(sum))
        .flatten()
}
pub(super) enum Drawing {
    Crown {
        origin: Instant,
        activity: Option<erlkonig_crown::WorkActivity>,
        reduced: bool,
        attention: bool,
    },
    Storage(Vec<(f64, u32)>),
    Progress(f64),
}
pub(super) struct DrawingState {
    drawing: RefCell<Drawing>,
    #[cfg(feature = "synthetic-proof")]
    geometry_proof: bool,
    #[cfg(feature = "synthetic-proof")]
    previous_geometry: RefCell<String>,
}
define_class!(
    #[unsafe(super(UIView))]
    #[name = "ErlkonigDrawing"]
    #[thread_kind = MainThreadOnly]
    #[ivars = DrawingState]
    pub(super) struct Canvas;
    unsafe impl NSObjectProtocol for Canvas {}
    impl Canvas {
        #[unsafe(method(drawRect:))]
        fn draw(&self, _dirty: CGRect) {
            let bounds = self.bounds();
            let (w, h) = (bounds.size.width, bounds.size.height);
            match &*self.ivars().drawing.borrow() {
                Drawing::Crown {
                    origin,
                    activity,
                    reduced,
                    attention,
                } => {
                    let frame = erlkonig_crown::frame(
                        origin.elapsed(),
                        *activity,
                        *reduced || UIAccessibilityIsReduceMotionEnabled(),
                        *attention,
                    );
                    let scale = (w / 26.).min(h / 18.) * 0.8;
                    let path = UIBezierPath::bezierPath();
                    for (i, p) in frame.points.iter().enumerate() {
                        let at = CGPoint::new(
                            (w - 26. * scale) / 2. + p.x * scale,
                            (h - 18. * scale) / 2. + p.y * scale,
                        );
                        if i == 0 {
                            path.moveToPoint(at)
                        } else {
                            path.addLineToPoint(at)
                        }
                    }
                    path.closePath();
                    rgb(0xf7f9ef).setFill();
                    path.fill();
                }
                Drawing::Storage(parts) => {
                    let mut x = 0.;
                    for &(fraction, color) in parts {
                        let width = w * fraction;
                        rgb(color).setFill();
                        UIBezierPath::bezierPathWithRect(rect(x, 0., width, h)).fill();
                        x += width;
                    }
                }
                Drawing::Progress(fraction) => {
                    let (left, right) = if objc2::available!(ios = 26.0) {
                        let Some(window) = self.window() else { return };
                        if self.convertRect_toView(bounds, Some(&window)) != window.bounds() {
                            return;
                        }
                        (
                            self.effectiveRadiusForCorner(UIRectCorner::BottomLeft),
                            self.effectiveRadiusForCorner(UIRectCorner::BottomRight),
                        )
                    } else {
                        (0., 0.)
                    };
                    if !left.is_finite() || !right.is_finite() || w <= 0. || h < 8. {
                        return;
                    }
                    UIColor::whiteColor().setFill();
                    let span = (w - left - right).max(0.);
                    let width = left + span * fraction.clamp(0., 1.);
                    UIBezierPath::bezierPathWithRect(rect(0., h - 8., width, 8.)).fill();
                    #[cfg(feature = "synthetic-proof")]
                    if self.ivars().geometry_proof && objc2::available!(ios = 26.0) {
                        let info=serde_json::json!({"frame_in_window":[bounds.origin.x,bounds.origin.y,w,h],"window_bounds":[bounds.origin.x,bounds.origin.y,w,h],
                                "bottom_left_radius":left,"bottom_right_radius":right,"track_width":span,"fraction":fraction,"strip_frame":[0.,h-8.,width,8.],
                                "system_version":UIDevice::currentDevice(self.mtm()).systemVersion().to_string()}).to_string();
                        if *self.ivars().previous_geometry.borrow() != info {
                            eprintln!("ERLKONIG_PROGRESS_GEOMETRY {info}");
                            *self.ivars().previous_geometry.borrow_mut() = info;
                        }
                    }
                }
            }
        }
    }
);
impl Canvas {
    pub fn create(mtm: MainThreadMarker, draw: Drawing, label: &str) -> Retained<Self> {
        let progress = matches!(draw, Drawing::Progress(_));
        let this = Self::alloc(mtm).set_ivars(DrawingState {
            drawing: RefCell::new(draw),
            #[cfg(feature = "synthetic-proof")]
            geometry_proof: progress && {
                let args: Vec<_> = std::env::args().collect();
                args.iter().any(|s| s == "--progress-geometry-proof")
                    && args.iter().any(|s| {
                        s.starts_with("--collections-fixture") || s.starts_with("--crown-fixture=")
                    })
            },
            #[cfg(feature = "synthetic-proof")]
            previous_geometry: Default::default(),
        });
        let this: Retained<Self> = unsafe { msg_send![super(this),initWithFrame:CGRect::ZERO] };
        this.setOpaque(false);
        this.setContentMode(UIViewContentMode::Redraw);
        this.setUserInteractionEnabled(false);
        this.setIsAccessibilityElement(true, mtm);
        this.setAccessibilityLabel(Some(&NSString::from_str(label)), mtm);
        if progress && objc2::available!(ios = 26.0) {
            this.setCornerConfiguration(&UICornerConfiguration::configurationWithRadius(
                &UICornerRadius::containerConcentricRadius(),
            ));
        }
        this
    }
    pub fn update(&self, drawing: Drawing) {
        *self.ivars().drawing.borrow_mut() = drawing;
        self.setNeedsDisplay();
    }
}
