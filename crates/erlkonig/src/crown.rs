//! Erlkonig's wavelet crown, still at rest and gently moving during app-owned work.
//!
//! The canonical website's Gabor/Morlet crown fixes the outer geometry and
//! reference band. Rest and motion share a softer, smooth-centred inner wave;
//! walls, tips and base stay put. Attention adds steady, sharper inner crests,
//! independently of work or Reduce Motion. No clock, timer, worker or network
//! lives here.
//!
//! The caller owns a monotonic origin and samples actual app-owned work, not
//! enabled services or payload throughput. Retain `began_at` throughout a burst
//! and finish its current cycle after work ends. Every point reaches its exact
//! resting position together at phase zero. New work before that landing keeps
//! the same phase. Passing `None` returns immediately to exact rest.

use std::f64::consts::TAU;
#[cfg(test)]
use std::f64::consts::PI;
use std::time::Duration;

pub const WIDTH: f64 = 26.0;
pub const HEIGHT: f64 = 18.0;
const ATTACK: f64 = 0.160;
const WAVE_STEPS: usize = 140;
const SCALE: f64 = 0.060; // 400 canonical units → 24 points, with 1 pt side padding
const WAVE_PERIOD: Duration = Duration::from_millis(3600);
// Normal rest and motion share one gain against the ORIGINAL canonical band.
const RIPPLE_AMPLITUDE: f64 = 0.75;
const CENTRE_RADIUS: f64 = 0.2;
const ATTENTION_AMPLITUDE: f64 = 0.95;
const ATTENTION_STATIC_WEIGHT: f64 = 0.70;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WorkActivity {
    pub began_at: Duration,
    pub ended_at: Option<Duration>,
}

/// Sample only actual owned work. Idle sampling preserves the pending landing
/// without extending it; the next burst restarts only after exact rest.
pub fn observe(activity: &mut Option<WorkActivity>, active: bool, now: Duration) {
    if active {
        let began_at = activity
            .filter(|a| !landed(now, *a))
            .map_or(now, |a| a.began_at);
        *activity = Some(WorkActivity { began_at, ended_at: None });
    } else if let Some(activity) = activity {
        activity.ended_at.get_or_insert(now);
    }
}

fn landed(now: Duration, activity: WorkActivity) -> bool {
    let Some(end) = activity.ended_at else {
        return false;
    };
    let Some(elapsed) = end.checked_sub(activity.began_at) else {
        return false;
    };
    let phase = elapsed.as_nanos() % WAVE_PERIOD.as_nanos();
    let remaining = (WAVE_PERIOD.as_nanos() - phase) % WAVE_PERIOD.as_nanos();
    now >= end && (now - end).as_nanos() >= remaining
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Point {
    pub x: f64,
    pub y: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Frame {
    /// Closed polygon in the 26 × 18 point template-image coordinate system.
    /// Coordinates are top-down, as on the canonical website.
    pub points: Vec<Point>,
    /// True during attack/work/landing; false means no further frames are needed
    /// until owned work, attention or the reduced-motion preference changes.
    pub animating: bool,
}

fn smooth(x: f64) -> f64 {
    let x = x.clamp(0.0, 1.0);
    x * x * (3.0 - 2.0 * x)
}

fn envelope(
    now: Duration,
    activity: Option<WorkActivity>,
    reduced_motion: bool,
) -> (f64, bool) {
    let Some(activity) = activity.filter(|_| !reduced_motion) else {
        return (0.0, false);
    };
    // Reject inconsistent snapshots, including timestamps from another origin.
    let end = activity.ended_at.unwrap_or(now);
    if activity.began_at > end || end > now {
        return (0.0, false);
    }
    if landed(now, activity) {
        return (0.0, false);
    }
    let attack = smooth((now - activity.began_at).as_secs_f64() / ATTACK);
    (attack, true)
}

// These parameters and the tangent-spire construction match erlkonig.ai's
// index.html. maxH is exactly 1: exp(-k*x²) <= 1, cos(x) <= 1, W(0) = 1.
fn wave_y(t: f64) -> f64 {
    let x = t / 0.25;
    186.0 - (-0.32 * x * x).exp() * (TAU * x).cos() * 155.0 * 0.45
}

fn wave_slope(t: f64) -> f64 {
    const H: f64 = 0.0003;
    (wave_y(t + H) - wave_y(t - H)) / (2.0 * H * 225.0)
}

fn template_point(x: f64, y: f64) -> Point {
    // Preserve the canonical 400 × 165.75 outline. The flat base is on a whole
    // point (also a whole Retina pixel), not between two antialiased rows.
    Point {
        x: WIDTH / 2.0 + (x - 250.0) * SCALE,
        y: 14.0 + (y - 282.0) * SCALE,
    }
}

fn wave_bounds(points: &[Point]) -> (f64, f64) {
    let wave = &points[2..WAVE_STEPS + 3];
    (
        wave.iter().map(|p| p.y).fold(f64::INFINITY, f64::min),
        wave.iter().map(|p| p.y).fold(f64::NEG_INFINITY, f64::max),
    )
}

// Broader phase spacing at the centre avoids a hairline trough between paired
// crests. Its derivative is zero at centre, so no quadrature correction is
// needed. Distance remains monotone and approaches ordinary spacing outward.
fn radial_phase(radius: f64) -> f64 {
    (radius * radius + CENTRE_RADIUS * CENTRE_RADIUS).sqrt() - CENTRE_RADIUS
}

fn radial_carrier(radius: f64, phase: f64) -> f64 {
    (TAU * radial_phase(radius) - phase).cos()
}

// A bounded triangular wave gives the same crown a few crisp bristles, not a
// warning glyph. Its phase never advances: attention itself is not activity.
fn attention_carrier(radius: f64) -> f64 {
    1.0 - 4.0 * (radius - radius.round()).abs()
}

fn canonical_points() -> Vec<Point> {
    let mut catch = 0.75;
    let mut steepest = f64::INFINITY;
    for step in 0..210 {
        let t = 0.625 + step as f64 * 0.0005;
        let slope = wave_slope(t);
        if slope < steepest {
            steepest = slope;
            catch = t;
        }
    }
    let catch_x = 250.0 + catch * 225.0;
    let apex_y = wave_y(catch) + wave_slope(catch) * (450.0 - catch_x);
    let mut points = Vec::with_capacity(WAVE_STEPS + 6);
    points.push(template_point(75.0, 282.0));
    points.push(template_point(50.0, apex_y));
    for i in 0..=WAVE_STEPS {
        let u = i as f64 / WAVE_STEPS as f64;
        let t = -catch + 2.0 * catch * u;
        points.push(template_point(250.0 + 225.0 * t, wave_y(t)));
    }
    points.push(template_point(450.0, apex_y));
    points.push(template_point(425.0, 282.0));
    points.push(points[0]);
    points
}

/// Pure monotonic time + observed activity + attention → vector frame.
///
/// Normal rest is the same softened wave at phase zero. Attention changes only
/// the shape, never the activity envelope. For either shape, reduced motion,
/// idle, invalid and fully settled inputs produce bit-identical resting points.
pub fn frame(
    now: Duration,
    activity: Option<WorkActivity>,
    reduced_motion: bool,
    needs_attention: bool,
) -> Frame {
    let (amount, animating) = envelope(now, activity, reduced_motion);
    let mut points = canonical_points();
    let (ceiling, floor) = wave_bounds(&points);
    let middle = (ceiling + floor) * 0.5;
    let half = (floor - ceiling) * 0.5;
    let phase = if amount > 0.0 {
        // Integer reduction retains phase precision after arbitrarily long
        // uptime. Each work burst starts the same ripple.
        let elapsed = now
            - activity
                .expect("positive envelope has valid activity")
                .began_at;
        TAU * (elapsed.as_nanos() % WAVE_PERIOD.as_nanos()) as f64
            / WAVE_PERIOD.as_nanos() as f64
    } else {
        0.0
    };
    for (i, point) in points[2..WAVE_STEPS + 3].iter_mut().enumerate() {
        let u = i as f64 / WAVE_STEPS as f64;
        // Pin each join AND its first segment. Only the outer 10% ramps;
        // the Gaussian alone softens the remaining wavelet lobes.
        let distance = u.min(1.0 - u);
        let pinned = 1.0 / WAVE_STEPS as f64;
        let taper = smooth((distance - pinned) / (0.10 - pinned));
        let radius = ((point.x - WIDTH / 2.0) / (225.0 * SCALE * 0.25)).abs();
        let rest = radial_carrier(radius, 0.0);
        let carrier = rest + amount * (radial_carrier(radius, phase) - rest);
        let (amplitude, carrier) = if needs_attention {
            // A convex mix keeps the canonical bounds without clipping. The
            // steady bristles remain readable while actual work can ripple
            // underneath; neither fault onset nor persistence starts motion.
            (
                ATTENTION_AMPLITUDE,
                ATTENTION_STATIC_WEIGHT * attention_carrier(radius)
                    + (1.0 - ATTENTION_STATIC_WEIGHT) * carrier,
            )
        } else {
            (RIPPLE_AMPLITUDE, carrier)
        };
        let target = middle
            - amplitude * half * (-0.32 * radius * radius).exp() * carrier;
        // One endpoint/tangent taper, one canonical reference. Neither the
        // softened rest nor motion is scaled twice; no clipping or blur.
        point.y += taper * (target - point.y);
    }
    Frame { points, animating }
}

impl Frame {
    /// Anti-aliased raster of the closed crown polygon as straight
    /// (non-premultiplied) RGBA8, `width_px × height_px`, row-major, top row
    /// first: the layout `tray_icon::Icon::from_rgba` takes. (tray-icon 0.26
    /// rotates each pixel to network-order ARGB for StatusNotifierItem and
    /// never premultiplies; see its `platform_impl/ksni/icon.rs`.) The 26 × 18
    /// point template geometry is scaled to the requested size on each axis.
    ///
    /// Coverage is exact along x and sampled on sixteen sub-scanlines per
    /// pixel row along y, under the nonzero winding rule. A pixel the polygon
    /// does not touch is entirely zero; every other pixel carries `rgba`'s
    /// colour unchanged with its alpha scaled by the coverage. No dependency,
    /// no intermediate bitmap: the same vector drives AppKit on macOS.
    pub fn rgba(&self, width_px: u32, height_px: u32, rgba: [u8; 4]) -> Vec<u8> {
        const SUBSAMPLES: usize = 16;
        let (width, height) = (width_px as usize, height_px as usize);
        let (scale_x, scale_y) = (width as f64 / WIDTH, height as f64 / HEIGHT);
        let mut pixels = vec![0_u8; width * height * 4];
        let mut coverage = vec![0.0_f64; width];
        let mut crossings: Vec<(f64, i32)> = Vec::new();
        for row in 0..height {
            coverage.fill(0.0);
            for sub in 0..SUBSAMPLES {
                let y = (row as f64 + (sub as f64 + 0.5) / SUBSAMPLES as f64) / scale_y;
                crossings.clear();
                for i in 0..self.points.len() {
                    let (a, b) = (self.points[i], self.points[(i + 1) % self.points.len()]);
                    // Half-open in y: a vertex on the scanline counts for one
                    // of its two edges, and horizontal edges never cross.
                    if (a.y <= y) == (b.y <= y) {
                        continue;
                    }
                    let x = a.x + (y - a.y) * (b.x - a.x) / (b.y - a.y);
                    crossings.push((x * scale_x, if a.y < b.y { 1 } else { -1 }));
                }
                crossings.sort_by(|p, q| p.0.total_cmp(&q.0));
                let (mut winding, mut start) = (0, 0.0);
                for &(x, direction) in &crossings {
                    if winding == 0 {
                        start = x;
                    }
                    winding += direction;
                    if winding != 0 {
                        continue;
                    }
                    let (left, right) = (start.max(0.0), x.min(width as f64));
                    if right <= left {
                        continue;
                    }
                    for column in left.floor() as usize..(right.ceil() as usize).min(width) {
                        let from = (column as f64).max(left);
                        let to = (column as f64 + 1.0).min(right);
                        coverage[column] += (to - from).max(0.0);
                    }
                }
            }
            for (column, covered) in coverage.iter().enumerate() {
                let alpha = (covered / SUBSAMPLES as f64).clamp(0.0, 1.0) * f64::from(rgba[3]);
                let alpha = alpha.round() as u8;
                if alpha == 0 {
                    continue;
                }
                let at = (row * width + column) * 4;
                pixels[at..at + 3].copy_from_slice(&rgba[..3]);
                pixels[at + 3] = alpha;
            }
        }
        pixels
    }
}

#[cfg(target_os = "macos")]
impl Frame {
    /// AppKit redraws this vector at the destination backing scale. There is
    /// no intermediate PNG, fixed-resolution bitmap, resampling or fake stroke.
    pub fn template_image(&self) -> objc2::rc::Retained<objc2_app_kit::NSImage> {
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
        let image = NSImage::imageWithSize_flipped_drawingHandler(
            NSSize::new(WIDTH, HEIGHT),
            true,
            &draw,
        );
        // Set this on EVERY frame, including transfer animation. AppKit owns
        // the tint for dark/light menu bars, selection and accessibility.
        image.setTemplate(true);
        image
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Keep the established normal-crown tests independent of fault inputs.
    fn frame(now: Duration, activity: Option<WorkActivity>, reduced_motion: bool) -> Frame {
        super::frame(now, activity, reduced_motion, false)
    }

    fn attention_frame(now: Duration, activity: Option<WorkActivity>, reduced_motion: bool) -> Frame {
        super::frame(now, activity, reduced_motion, true)
    }

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    fn burst(start: u64, last: u64) -> Option<WorkActivity> {
        Some(WorkActivity {
            began_at: ms(start),
            ended_at: Some(ms(last)),
        })
    }

    #[test]
    fn idle_is_exact_at_every_time_and_after_settle() {
        let idle = frame(Duration::ZERO, None, false);
        assert!(!idle.animating);
        for now in [ms(1), ms(10_000), Duration::MAX] {
            assert_eq!(idle, frame(now, None, false));
        }
        assert_eq!(idle, frame(ms(3600), burst(0, 1000), false));
        assert_eq!(idle, frame(Duration::MAX, burst(0, 0), false));
    }

    #[test]
    fn reduced_motion_is_exactly_static_during_work() {
        let idle = frame(ms(0), None, false);
        for now in [0, 80, 160, 1000, 10_000] {
            assert_eq!(idle, frame(ms(now), burst(0, now), true));
        }
    }

    #[test]
    fn normal_shape_remains_the_original_softened_wave_at_every_phase() {
        let activity = Some(WorkActivity { began_at: ms(0), ended_at: None });
        for tick in 0..720 {
            let now = ms(tick * 5);
            let actual = frame(now, activity, false);
            let mut original = canonical_points();
            let (ceiling, floor) = wave_bounds(&original);
            let middle = (ceiling + floor) * 0.5;
            let half = (floor - ceiling) * 0.5;
            let (amount, animating) = envelope(now, activity, false);
            let phase = TAU * (now.as_nanos() % WAVE_PERIOD.as_nanos()) as f64
                / WAVE_PERIOD.as_nanos() as f64;
            for (i, point) in original[2..WAVE_STEPS + 3].iter_mut().enumerate() {
                let u = i as f64 / WAVE_STEPS as f64;
                let pinned = 1.0 / WAVE_STEPS as f64;
                let taper = smooth((u.min(1.0 - u) - pinned) / (0.10 - pinned));
                let radius = ((point.x - WIDTH / 2.0) / (225.0 * SCALE * 0.25)).abs();
                let rest = radial_carrier(radius, 0.0);
                let carrier = rest + amount * (radial_carrier(radius, phase) - rest);
                let target = middle
                    - RIPPLE_AMPLITUDE * half * (-0.32 * radius * radius).exp() * carrier;
                point.y += taper * (target - point.y);
            }
            assert_eq!(actual, Frame { points: original, animating });
        }
    }

    #[test]
    fn attention_is_static_without_work_and_survives_reduced_motion() {
        let idle = frame(ms(0), None, false);
        let attention = attention_frame(ms(0), None, false);
        assert_ne!(attention.points, idle.points);
        assert!(!attention.animating);
        for now in [ms(0), ms(80), ms(1000), ms(10_000), Duration::MAX] {
            assert_eq!(attention, attention_frame(now, None, false));
            assert_eq!(attention, attention_frame(now, None, true));
            let active = Some(WorkActivity { began_at: ms(0), ended_at: None });
            assert_eq!(attention, attention_frame(now, active, true));
        }
        assert_eq!(attention, attention_frame(ms(3600), burst(0, 1000), false));
        assert_eq!(attention, attention_frame(Duration::MAX, burst(0, 1000), false));
        assert_eq!(attention, attention_frame(ms(1000), burst(1001, 1001), false));
        assert_eq!(attention, attention_frame(ms(1000), burst(900, 800), false));
    }

    #[test]
    fn attention_is_noticeably_sharper_but_preserves_the_crown() {
        let idle = frame(ms(0), None, false);
        let attention = attention_frame(ms(0), None, false);
        let centre = 2 + WAVE_STEPS / 2;
        assert!(idle.points[centre].y - attention.points[centre].y > 0.75);
        let normal_rise = idle.points[centre + 1].y - idle.points[centre].y;
        let sharp_rise = attention.points[centre + 1].y - attention.points[centre].y;
        assert!(sharp_rise > 0.35 && sharp_rise > 10.0 * normal_rise);
        assert!(idle.points.iter().zip(&attention.points)
            .filter(|(a, b)| (a.y - b.y).abs() > 0.5).count() > 30);
        for radius in [0.0, 0.25, 0.5, 0.75, 1.0, 1.5, 2.0, 2.5] {
            assert!(attention_carrier(radius).abs() <= 1.0);
        }
    }

    #[test]
    fn attention_keeps_finite_bounds_symmetry_and_pinned_endpoints_during_work() {
        let canonical = canonical_points();
        let (ceiling, floor) = wave_bounds(&canonical);
        for tick in 0..1200 {
            let now = ms(tick * 10);
            let active = Some(WorkActivity { began_at: ms(0), ended_at: None });
            let normal = frame(now, active, false);
            let attention = attention_frame(now, active, false);
            assert_eq!(attention.animating, normal.animating);
            assert_eq!(attention.points.first(), attention.points.last());
            for (i, p) in attention.points.iter().enumerate() {
                assert!(p.x.is_finite() && p.y.is_finite());
                assert!((1.0..=25.0).contains(&p.x));
                assert!((2.0..=15.0).contains(&p.y));
                assert_eq!(p.x, canonical[i].x);
                if (2..WAVE_STEPS + 3).contains(&i) {
                    assert!(p.y >= ceiling - 1e-12 && p.y <= floor + 1e-12);
                    assert!((p.y - attention.points[WAVE_STEPS + 4 - i].y).abs() < 1e-12);
                }
            }
            for i in [0, 1, 2, 3, 141, 142, 143, 144, 145] {
                assert_eq!(attention.points[i], canonical[i]);
            }
        }
    }

    #[test]
    fn actual_work_can_ripple_under_attention_and_lands_on_the_same_static_shape() {
        let attention = attention_frame(ms(0), None, false);
        let activity = Some(WorkActivity { began_at: ms(0), ended_at: None });
        let first = attention_frame(ms(0), activity, false);
        assert!(first.animating);
        assert_eq!(first.points, attention.points);
        assert_ne!(attention_frame(ms(900), activity, false).points, attention.points);
        assert_ne!(attention_frame(ms(900), activity, false).points,
            attention_frame(ms(1800), activity, false).points);
        assert_eq!(attention_frame(ms(3600), activity, false).points, attention.points);
        let stopped = burst(0, 1000);
        let almost = attention_frame(ms(3599), stopped, false);
        assert!(almost.animating);
        assert!(almost.points.iter().zip(&attention.points)
            .all(|(a, b)| (a.y - b.y).abs() < 0.02));
        assert_eq!(attention_frame(ms(3600), stopped, false), attention);
    }

    #[test]
    fn attack_is_continuous_and_landing_finishes_at_neutral() {
        let idle = frame(ms(0), None, false);
        let first = frame(ms(1000), burst(1000, 1000), false);
        assert_eq!(first, idle); // Work ending immediately is already neutral.
        let active = Some(WorkActivity { began_at: ms(1000), ended_at: None });
        let first = frame(ms(1000), active, false);
        assert!(first.animating);
        assert_eq!(first.points, idle.points);
        assert_ne!(frame(ms(1160), active, false).points, idle.points);
        let stopping = burst(1000, 1160);
        assert_eq!(frame(ms(2000), stopping, false), frame(ms(2000), active, false));
        let almost = frame(ms(4599), stopping, false);
        assert!(almost.animating);
        assert!(almost.points.iter().zip(&idle.points)
            .all(|(a, b)| (a.y - b.y).abs() < 0.02));
        assert_eq!(idle, frame(ms(4600), stopping, false));
        assert_eq!(idle, frame(Duration::MAX, stopping, false));
    }

    #[test]
    fn every_stop_phase_lands_within_one_cycle_at_exact_rest() {
        let idle = frame(ms(0), None, false);
        for stopped in (0_u64..7201).step_by(17).chain([0, 3600, 7200]) {
            let activity = burst(0, stopped);
            let landing = stopped.div_ceil(3600) * 3600;
            assert!(landing - stopped < 3600);
            assert_eq!(frame(ms(landing), activity, false), idle);
            if landing > stopped {
                assert!(frame(ms(landing - 1), activity, false).animating);
            }
        }
    }

    #[test]
    fn new_work_cancels_pending_landing_without_phase_or_position_jump() {
        let mut activity = None;
        observe(&mut activity, true, ms(1000));
        observe(&mut activity, false, ms(2000));
        let before = frame(ms(4200), activity, false);
        observe(&mut activity, true, ms(4200));
        assert_eq!(before, frame(ms(4200), activity, false));
        assert_eq!(activity.unwrap().began_at, ms(1000));
        assert!(frame(ms(4600), activity, false).animating);
        observe(&mut activity, false, ms(4700));
        observe(&mut activity, false, ms(7900));
        assert_eq!(activity.unwrap().ended_at, Some(ms(4700)));
        assert!(!frame(ms(8200), activity, false).animating);
        observe(&mut activity, true, ms(8300));
        assert_eq!(activity.unwrap().began_at, ms(8300));
        assert_eq!(frame(ms(8300), activity, false).points, frame(ms(0), None, false).points);
    }

    #[test]
    fn neutral_landing_keeps_integer_precision_at_long_uptime() {
        let origin = Duration::MAX - ms(10_000);
        let activity = Some(WorkActivity {
            began_at: origin,
            ended_at: Some(origin + ms(1000)),
        });
        assert!(frame(origin + ms(3599), activity, false).animating);
        assert_eq!(frame(origin + ms(3600), activity, false), frame(ms(0), None, false));
        assert_eq!(frame(Duration::MAX, activity, false), frame(ms(0), None, false));
    }

    #[test]
    fn coordinates_are_finite_bounded_and_outer_silhouette_does_not_move() {
        let idle = frame(ms(0), None, false);
        let canonical = canonical_points();
        let (ceiling, floor) = wave_bounds(&canonical);
        for tick in 0..1200 {
            let now = ms(tick * 10);
            let f = frame(
                now,
                Some(WorkActivity {
                    began_at: Duration::ZERO,
                    ended_at: None,
                }),
                false,
            );
            assert_eq!(f.points.first(), f.points.last());
            for (i, p) in f.points.iter().enumerate() {
                assert!(p.x.is_finite() && p.y.is_finite());
                assert!((1.0..=25.0).contains(&p.x));
                assert!((2.0..=15.0).contains(&p.y));
                assert_eq!(p.x, idle.points[i].x);
                if (2..WAVE_STEPS + 3).contains(&i) {
                    assert!(p.y >= ceiling - 1e-12 && p.y <= floor + 1e-12);
                    assert!((p.y - f.points[WAVE_STEPS + 4 - i].y).abs() < 1e-12);
                }
                if i <= 2 || i >= WAVE_STEPS + 2 {
                    assert_eq!(*p, canonical[i]);
                }
            }
        }
    }

    #[test]
    fn deterministic_inputs_and_long_uptime_keep_the_same_carrier() {
        let early = frame(ms(2234), burst(0, 2234), false);
        assert_eq!(early, frame(ms(2234), burst(0, 2234), false));
        // Eighteen whole seconds are exactly five 3.6 s periods.
        let late = Duration::new(u64::MAX - u64::MAX % 18 + 2, 234_000_000);
        let f = frame(
            late,
            Some(WorkActivity {
                began_at: Duration::ZERO,
                ended_at: None,
            }),
            false,
        );
        assert_eq!(early, f);
    }

    #[test]
    fn inconsistent_timestamps_do_not_invent_activity() {
        let idle = frame(ms(0), None, false);
        assert_eq!(idle, frame(ms(1000), burst(1001, 1001), false));
        assert_eq!(idle, frame(ms(1000), burst(900, 800), false));
    }

    #[test]
    fn native_geometry_has_softened_rest_and_pinned_outer_outline() {
        let idle = frame(ms(0), None, false);
        let min_x = idle.points.iter().map(|p| p.x).fold(f64::INFINITY, f64::min);
        let max_x = idle.points.iter().map(|p| p.x).fold(f64::NEG_INFINITY, f64::max);
        let min_y = idle.points.iter().map(|p| p.y).fold(f64::INFINITY, f64::min);
        let max_y = idle.points.iter().map(|p| p.y).fold(f64::NEG_INFINITY, f64::max);
        assert_eq!((min_x, max_x), (1.0, 25.0));
        assert_eq!(max_y, 14.0);
        let (ceiling, floor) = wave_bounds(&canonical_points());
        assert!((ceiling - 4.055).abs() < 1e-12);
        assert!((floor - 12.107434011801187).abs() < 1e-12);
        assert!((min_y - ((ceiling + floor) * 0.5 - RIPPLE_AMPLITUDE * (floor - ceiling) * 0.5)).abs() < 1e-12);
        assert!(min_y > ceiling);
    }

    #[test]
    fn ripple_phase_inverts_and_travels_outward_without_a_cusp() {
        for radius in [0.0, 0.01, 0.06, 0.12, 0.3, 0.8, 2.0] {
            for i in 0..600 {
                let phase = TAU * i as f64 / 600.0;
                let carrier = radial_carrier(radius, phase);
                assert!(carrier.abs() <= 1.0 + 1e-12);
                assert!((carrier + radial_carrier(radius, phase + PI)).abs() < 1e-12);
            }
        }
        for radius in [0.2, 0.4, 0.7, 1.0] {
            let advance = radial_phase(radius + 0.1) - radial_phase(radius);
            assert!(advance > 0.0 && advance < 0.1);
            assert!(
                (radial_carrier(radius, 0.3) - radial_carrier(radius + 0.1, 0.3 + TAU * advance)).abs()
                    < 1e-12
            );
        }
        for phase in [0.0, PI * 0.5, PI, PI * 1.5] {
            assert!(
                ((radial_carrier(1e-7, phase) - radial_carrier(0.0, phase)) / 1e-7).abs() < 0.00001
            );
        }
    }

    #[test]
    fn active_ripple_uses_a_softer_centred_band_without_deepening() {
        let idle = frame(ms(0), None, false);
        let (ceiling, floor) = wave_bounds(&canonical_points());
        let crest = frame(ms(3600), burst(0, 3600), false);
        let quarter = frame(ms(900), burst(0, 900), false);
        let valley = frame(ms(1800), burst(0, 1800), false);
        let centre = 2 + WAVE_STEPS / 2;
        let middle = (ceiling + floor) * 0.5;
        let amplitude = (floor - ceiling) * 0.5 * RIPPLE_AMPLITUDE;
        assert!((crest.points[centre].y - (middle - amplitude)).abs() < 1e-12);
        assert!((quarter.points[centre].y - middle).abs() < 1e-12);
        assert!((valley.points[centre].y - (middle + amplitude)).abs() < 1e-12);
        assert!(crest.points[centre].y > ceiling && valley.points[centre].y < floor);
        assert_eq!(crest.points, idle.points); // rest is the SAME phase-zero wave
        assert_eq!(quarter, frame(ms(4500), burst(0, 4500), false));
        // Phase is relative to each burst, not an arbitrary app uptime origin.
        assert_eq!(valley, frame(ms(11_800), burst(10_000, 11_800), false));
    }

    #[test]
    fn central_inversion_has_a_broader_opening_between_crests() {
        let f = frame(ms(1800), burst(0, 1800), false);
        let centre = 2 + WAVE_STEPS / 2;
        let mut peak = centre + 1;
        while f.points[peak + 1].y < f.points[peak].y { peak += 1; }
        assert!(2.0 * (f.points[peak].x - WIDTH / 2.0) > 4.0);
        let level = (f.points[centre].y + f.points[peak].y) * 0.5;
        let mut right = centre + 1;
        while f.points[right].y > level { right += 1; }
        let (a, b) = (f.points[right - 1], f.points[right]);
        let crossing = a.x + (level - a.y) * (b.x - a.x) / (b.y - a.y);
        // v9's half-depth opening was only 1.543 pt; this is not just a
        // shallower copy of that hairline valley or a raster blur.
        assert!(2.0 * (crossing - WIDTH / 2.0) > 2.0);
    }

    #[test]
    fn work_samples_hold_long_passes_and_idle_never_rearms() {
        let idle = frame(ms(0), None, false);
        let mut activity = None;
        for now in [0, 250, 1000, 60_000] {
            observe(&mut activity, false, ms(now));
            assert_eq!(idle, frame(ms(now), activity, false));
        }
        for now in (61_000..70_000).step_by(250) {
            observe(&mut activity, true, ms(now));
            assert!(frame(ms(now + 249), activity, false).animating);
            assert_eq!(activity.unwrap().began_at, ms(61_000));
            assert_eq!(idle, frame(ms(now), activity, true));
        }
        // A delayed status sample cannot sag or restart an ongoing scope.
        assert_eq!(envelope(ms(75_000), activity, false), (1.0, true));
        observe(&mut activity, true, ms(75_000));
        assert_eq!(activity.unwrap().began_at, ms(61_000));
        observe(&mut activity, false, ms(76_000));
        assert!(frame(ms(78_999), activity, false).animating);
        for now in [ms(79_000), ms(80_000), ms(90_000)] {
            observe(&mut activity, false, now);
            assert_eq!(idle, frame(now, activity, false));
        }
        observe(&mut activity, true, ms(91_000));
        assert_eq!(activity.unwrap().began_at, ms(91_000));
        assert_eq!(frame(ms(91_000), activity, false).points, idle.points);
    }

    #[test]
    fn endpoint_taper_pins_first_segments_and_releases_outer_lobes() {
        let base = canonical_points();
        for tick in 0..720 {
            let now = ms(tick * 5);
            let f = frame(now, burst(0, tick * 5), false);
            for i in [0, 1, 2, 3, 141, 142, 143, 144, 145] {
                assert_eq!(f.points[i], base[i]);
            }
        }
        // The former global taper reduced this second radial crest to ~0.10 pt.
        let index = (2..143).min_by(|&a, &b|
            (base[a].x - 20.394255).abs().total_cmp(&(base[b].x - 20.394255).abs())
        ).unwrap();
        let mut low = f64::INFINITY;
        let mut high = f64::NEG_INFINITY;
        for tick in 120..240 {
            let f = frame(ms(tick * 30), burst(0, tick * 30), false);
            low = low.min(f.points[index].y);
            high = high.max(f.points[index].y);
        }
        let u = (index - 2) as f64 / WAVE_STEPS as f64;
        let radius = ((base[index].x - WIDTH / 2.0) / (225.0 * SCALE * 0.25)).abs();
        let taper = smooth((u.min(1.0 - u) - 1.0 / 140.0) / (0.10 - 1.0 / 140.0));
        let (ceiling, floor) = wave_bounds(&base);
        let expected = (floor - ceiling) * RIPPLE_AMPLITUDE
            * (-0.32 * radius * radius).exp() * taper;
        assert!((high - low - expected).abs() < 0.003); // 120 sampled phases
        assert!(high - low > 1.0); // At least two native Retina pixels of travel.
    }

    // The native proof in scripts/crown-render.rs renders these ten frames at
    // 1× and 2× and inspects the AppKit alpha mask. The portable raster must
    // satisfy the same assertions.
    const PROOF_CASES: [(&str, u64, Option<u64>, bool, bool); 10] = [
        ("idle", 0, None, false, false),
        ("attack", 80, Some(80), false, false),
        ("working", 1000, Some(1000), false, false),
        ("landing", 3599, Some(1000), false, false),
        ("settled", 3600, Some(1000), false, false),
        ("reduced", 1000, Some(1000), true, false),
        ("attention", 0, None, false, true),
        ("attention-working", 1000, Some(1000), false, true),
        ("attention-settled", 3600, Some(1000), false, true),
        ("attention-reduced", 1000, Some(1000), true, true),
    ];

    fn proof_frame(now: u64, last: Option<u64>, reduced: bool, attention: bool) -> Frame {
        let activity = last.map(|last| WorkActivity {
            began_at: Duration::ZERO,
            ended_at: Some(ms(last)),
        });
        super::frame(ms(now), activity, reduced, attention)
    }

    fn shoelace_area(points: &[Point]) -> f64 {
        let mut twice = 0.0;
        for i in 0..points.len() {
            let (a, b) = (points[i], points[(i + 1) % points.len()]);
            twice += a.x * b.y - b.x * a.y;
        }
        twice.abs() * 0.5
    }

    #[test]
    fn rgba_mirrors_the_native_mask_proof() {
        let black = [0, 0, 0, 255];
        let mut idle_pixels = Vec::new();
        let mut attention_pixels = Vec::new();
        for (label, now, last, reduced, attention) in PROOF_CASES {
            let frame = proof_frame(now, last, reduced, attention);
            for scale in [1_u32, 2] {
                let (width, height) = (26 * scale as usize, 18 * scale as usize);
                let pixels = frame.rgba(26 * scale, 18 * scale, black);
                assert_eq!(pixels.len(), width * height * 4);
                assert!(pixels.chunks_exact(4).all(|p| p[..3] == [0, 0, 0]));
                let solid = pixels.chunks_exact(4).filter(|p| p[3] == 255).count();
                let partial = pixels
                    .chunks_exact(4)
                    .filter(|p| p[3] > 0 && p[3] < 255)
                    .count();
                assert!(
                    solid > 0 && partial > 0,
                    "{label} {scale}x: {solid} solid, {partial} partial"
                );
                for row in 0..height {
                    assert_eq!(pixels[(row * width) * 4 + 3], 0);
                    assert_eq!(pixels[(row * width + width - 1) * 4 + 3], 0);
                }
                assert!(pixels[..width * 4].iter().all(|p| *p == 0));
                assert!(pixels[(height - 1) * width * 4..].iter().all(|p| *p == 0));
                if scale == 2 {
                    match label {
                        "idle" => idle_pixels = pixels,
                        "settled" | "reduced" => assert_eq!(pixels, idle_pixels, "{label}"),
                        "working" => assert_ne!(pixels, idle_pixels),
                        "attention" => {
                            assert_ne!(pixels, idle_pixels);
                            attention_pixels = pixels;
                        }
                        "attention-settled" | "attention-reduced" => {
                            assert_eq!(pixels, attention_pixels, "{label}");
                        }
                        "attention-working" => assert_ne!(pixels, attention_pixels),
                        _ => {}
                    }
                }
            }
        }
    }

    #[test]
    fn rgba_is_mirror_symmetric_like_the_geometry() {
        for (label, now, last, reduced, attention) in PROOF_CASES {
            let frame = proof_frame(now, last, reduced, attention);
            for (width, height) in [(26_u32, 18_u32), (52, 36), (32, 32), (24, 24)] {
                let pixels = frame.rgba(width, height, [255, 255, 255, 255]);
                let width = width as usize;
                for (i, pixel) in pixels.chunks_exact(4).enumerate() {
                    let (row, column) = (i / width, i % width);
                    let mirror = pixels[(row * width + width - 1 - column) * 4 + 3];
                    // Mirror-image coverage differs only by floating-point rounding.
                    assert!(
                        pixel[3].abs_diff(mirror) <= 1,
                        "{label} {width}×{height} ({column},{row})"
                    );
                }
            }
        }
    }

    #[test]
    fn rgba_keeps_straight_colour_and_scales_alpha_by_coverage() {
        let frame = frame(ms(0), None, false);
        let opaque = frame.rgba(52, 36, [10, 200, 30, 255]);
        let translucent = frame.rgba(52, 36, [10, 200, 30, 128]);
        assert_eq!(opaque.len(), translucent.len());
        let mut touched = 0;
        for (a, b) in opaque.chunks_exact(4).zip(translucent.chunks_exact(4)) {
            if a[3] == 0 {
                // Untouched pixels are entirely zero, not a transparent colour.
                assert_eq!(a, [0, 0, 0, 0]);
                assert_eq!(b, [0, 0, 0, 0]);
                continue;
            }
            touched += 1;
            // Straight alpha: the colour is never premultiplied by coverage.
            assert_eq!(a[..3], [10, 200, 30]);
            assert_eq!(b[..3], [10, 200, 30]);
            assert!(b[3] <= 128);
            assert!((f64::from(b[3]) - f64::from(a[3]) * 128.0 / 255.0).abs() <= 1.0);
        }
        assert!(touched > 100);
    }

    #[test]
    fn rgba_coverage_integrates_to_the_polygon_area() {
        for (label, now, last, reduced, attention) in PROOF_CASES {
            let frame = proof_frame(now, last, reduced, attention);
            let area = shoelace_area(&frame.points);
            assert!(area > 100.0 && area < 250.0, "{label}: {area}");
            for (width, height) in [(26_u32, 18_u32), (52, 36), (104, 72), (52, 18)] {
                let pixels = frame.rgba(width, height, [0, 0, 0, 255]);
                let covered: f64 = pixels
                    .chunks_exact(4)
                    .map(|p| f64::from(p[3]) / 255.0)
                    .sum();
                let expected = area * f64::from(width) / WIDTH * f64::from(height) / HEIGHT;
                assert!(
                    (covered - expected).abs() < 0.01 * expected,
                    "{label} {width}×{height}: covered {covered}, polygon {expected}"
                );
            }
        }
    }
}
