//! Erlkönig's wavelet crown, still at rest and gently moving during byte transfer.
//!
//! The canonical website's Gabor/Morlet crown fixes the outer geometry and
//! reference band. Rest and motion share a softer, smooth-centred inner wave;
//! walls, tips and base stay put. No clock, timer, worker or network lives here.
//!
//! The caller owns a monotonic origin and updates `last_transfer_at` ONLY when
//! received/sent byte counters increase, not when a peer connects or a poll
//! succeeds. Retain `began_at` throughout a burst; start a new burst after
//! `SETTLE_AFTER` without bytes. Keep the last burst until it settles: passing
//! `None` intentionally returns immediately to the exact resting outline.

use std::f64::consts::{PI, TAU};
use std::time::Duration;

pub const WIDTH: f64 = 26.0;
pub const HEIGHT: f64 = 18.0;
/// After this much time without bytes the exact resting crown is restored.
pub const SETTLE_AFTER: Duration = Duration::from_millis(600);
const ATTACK: f64 = 0.160;
// Runtime publishes measured byte deltas at ~250 ms cadence. Hold an observed
// pulse across a normal sample interval; unchanged counters never renew it.
const HOLD: f64 = 0.300;
const RELEASE: f64 = 0.300;
const WAVE_STEPS: usize = 140;
const SCALE: f64 = 0.060; // 400 canonical units → 24 points, with 1 pt side padding
const WAVE_PERIOD: Duration = Duration::from_millis(3600);
// One gain against the ORIGINAL canonical band, for both rest and motion.
const RIPPLE_AMPLITUDE: f64 = 0.75;
const CENTRE_RADIUS: f64 = 0.2;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TransferActivity {
    pub began_at: Duration,
    pub last_transfer_at: Duration,
}

/// Session-local visual intent, separate from observed byte activity.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Preview {
    began_at: Option<Duration>,
    stopped_at: Option<Duration>,
}

impl Preview {
    pub fn is_enabled(&self) -> bool {
        self.began_at.is_some() && self.stopped_at.is_none()
    }

    pub fn set_enabled(&mut self, enabled: bool, now: Duration) {
        if enabled {
            self.began_at = Some(now);
            self.stopped_at = None;
        } else if self.is_enabled() {
            self.stopped_at = Some(now);
        }
    }

    fn activity(&self, now: Duration) -> Option<TransferActivity> {
        let began_at = self.began_at?;
        let last_transfer_at = self.stopped_at.unwrap_or(now);
        (began_at <= last_transfer_at
            && last_transfer_at <= now
            && now - last_transfer_at < SETTLE_AFTER)
            .then_some(TransferActivity { began_at, last_transfer_at })
    }
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
    /// True during attack/hold/settle; false means no further frames are needed
    /// until byte activity or the reduced-motion preference changes.
    pub animating: bool,
}

fn smooth(x: f64) -> f64 {
    let x = x.clamp(0.0, 1.0);
    x * x * (3.0 - 2.0 * x)
}

fn envelope(
    now: Duration,
    activity: Option<TransferActivity>,
    reduced_motion: bool,
) -> (f64, bool) {
    let Some(activity) = activity.filter(|_| !reduced_motion) else {
        return (0.0, false);
    };
    // Reject inconsistent snapshots, including timestamps from another origin.
    if activity.began_at > activity.last_transfer_at || activity.last_transfer_at > now {
        return (0.0, false);
    }
    let quiet = now - activity.last_transfer_at;
    if quiet >= SETTLE_AFTER {
        return (0.0, false);
    }
    let attack = smooth((now - activity.began_at).as_secs_f64() / ATTACK);
    let release = 1.0 - smooth((quiet.as_secs_f64() - HOLD) / RELEASE);
    (attack * release, true)
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

/// Pure monotonic time + observed activity → vector frame.
///
/// Rest is the same softened wave at phase zero. Reduced motion, idle, invalid
/// and fully settled inputs all produce bit-identical resting points.
pub fn frame(now: Duration, activity: Option<TransferActivity>, reduced_motion: bool) -> Frame {
    let (amount, animating) = envelope(now, activity, reduced_motion);
    let mut points = canonical_points();
    let (ceiling, floor) = wave_bounds(&points);
    let middle = (ceiling + floor) * 0.5;
    let half = (floor - ceiling) * 0.5;
    let phase = if amount > 0.0 {
        // Integer reduction retains phase precision after arbitrarily long
        // uptime. Each real burst or explicit preview starts the same ripple.
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
        let target = middle
            - RIPPLE_AMPLITUDE * half * (-0.32 * radius * radius).exp() * carrier;
        // One endpoint/tangent taper, one canonical reference. Neither the
        // softened rest nor motion is scaled twice; no clipping or blur.
        point.y += taper * (target - point.y);
    }
    Frame { points, animating }
}

/// Select visual-only preview intent without changing observed byte activity.
///
/// ON sustains the preview. OFF crossfades its settling tail into the genuine
/// frame, reaching that exact frame after 600 ms without a phase/attack snap.
pub fn frame_with_preview(
    now: Duration,
    activity: Option<TransferActivity>,
    preview: Preview,
    reduced_motion: bool,
) -> Frame {
    let real = frame(now, activity, reduced_motion);
    if reduced_motion {
        return real;
    }
    let Some(visual_activity) = preview.activity(now) else {
        return real;
    };
    let mut visual = frame(now, Some(visual_activity), false);
    if let Some(stopped_at) = preview.stopped_at {
        let mix = smooth((now - stopped_at).as_secs_f64() / SETTLE_AFTER.as_secs_f64());
        for (point, target) in visual.points.iter_mut().zip(&real.points) {
            point.y += mix * (target.y - point.y);
        }
        visual.animating |= real.animating;
    }
    visual
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

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    fn burst(start: u64, last: u64) -> Option<TransferActivity> {
        Some(TransferActivity {
            began_at: ms(start),
            last_transfer_at: ms(last),
        })
    }

    #[test]
    fn idle_is_exact_at_every_time_and_after_settle() {
        let idle = frame(Duration::ZERO, None, false);
        assert!(!idle.animating);
        for now in [ms(1), ms(10_000), Duration::MAX] {
            assert_eq!(idle, frame(now, None, false));
        }
        assert_eq!(idle, frame(ms(1600), burst(0, 1000), false));
        assert_eq!(idle, frame(Duration::MAX, burst(0, 0), false));
    }

    #[test]
    fn reduced_motion_is_exactly_static_during_transfer() {
        let idle = frame(ms(0), None, false);
        for now in [0, 80, 160, 1000, 10_000] {
            assert_eq!(idle, frame(ms(now), burst(0, now), true));
        }
    }

    #[test]
    fn attack_and_release_are_continuous_and_finish_exactly() {
        let idle = frame(ms(0), None, false);
        let first = frame(ms(1000), burst(1000, 1000), false);
        assert!(first.animating); // caller must schedule the attack's next frame
        assert_eq!(first.points, idle.points);
        let active = frame(ms(1160), burst(1000, 1160), false);
        assert!(active.animating);
        assert_ne!(active.points, idle.points);
        let almost = frame(ms(1759), burst(1000, 1160), false);
        assert!(almost.animating);
        let (ceiling, floor) = wave_bounds(&canonical_points());
        // One millisecond before the smoothstep ends, less than 1.4e-5 of
        // the motion remains. Keep the tail check independent of amplitude.
        assert!(
            almost
                .points
                .iter()
                .zip(&idle.points)
                .all(|(a, b)| (a.y - b.y).abs() < (floor - ceiling) * RIPPLE_AMPLITUDE * 0.000014)
        );
        assert_eq!(idle, frame(ms(1760), burst(1000, 1160), false));
        assert_eq!(idle, frame(ms(2000), burst(1000, 1160), false));
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
                Some(TransferActivity {
                    began_at: Duration::ZERO,
                    last_transfer_at: now,
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
            Some(TransferActivity {
                began_at: Duration::ZERO,
                last_transfer_at: late,
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
    fn preview_stays_on_until_off_then_settles_exactly() {
        let idle = frame(ms(0), None, false);
        let mut preview = Preview::default();
        assert!(!preview.is_enabled());
        preview.set_enabled(true, ms(1000));
        for elapsed in [0, 80, 6000, 60_000, 86_400_000] {
            assert!(frame_with_preview(ms(1000 + elapsed), None, preview, false).animating);
        }
        assert!(preview.is_enabled());
        assert_ne!(
            idle.points,
            frame_with_preview(ms(2000), None, preview, false).points
        );
        preview.set_enabled(false, ms(7000));
        assert!(!preview.is_enabled());
        assert!(frame_with_preview(ms(7599), None, preview, false).animating);
        for now in [ms(7600), ms(9000), Duration::MAX] {
            assert_eq!(idle, frame_with_preview(now, None, preview, false));
        }
        preview.set_enabled(false, ms(9000)); // Repeated OFF never extends settling.
        assert_eq!(idle, frame_with_preview(ms(9000), None, preview, false));
    }

    #[test]
    fn preview_retrigger_restarts_one_envelope() {
        let mut preview = Preview::default();
        preview.set_enabled(true, ms(1000));
        preview.set_enabled(false, ms(6000));
        assert!(frame_with_preview(ms(6500), None, preview, false).animating);
        preview.set_enabled(true, ms(6500));
        let restarted = frame_with_preview(ms(6500), None, preview, false);
        assert!(restarted.animating);
        assert_eq!(restarted.points, frame(ms(0), None, false).points);
        assert!(frame_with_preview(ms(13_100), None, preview, false).animating);
    }

    #[test]
    fn preview_respects_reduced_motion_at_every_phase() {
        let idle = frame(ms(0), None, false);
        let mut preview = Preview::default();
        preview.set_enabled(true, ms(1000));
        for now in [1000, 1080, 3000, 7000, 7599, 7600] {
            assert_eq!(
                idle,
                frame_with_preview(ms(now), burst(0, now), preview, true)
            );
            assert!(preview.is_enabled()); // Checkmark is intent, not animation.
        }
        preview.set_enabled(false, ms(7600));
        assert_eq!(idle, frame_with_preview(ms(7601), None, preview, true));
    }

    #[test]
    fn preview_never_leaks_into_real_activity() {
        let mut preview = Preview::default();
        preview.set_enabled(true, ms(1000));
        preview.set_enabled(false, ms(7000));
        // The genuine frame is exact after the visual-only settling tail.
        for now in [ms(7600), ms(10_000)] {
            for real in [
                burst(7000, now.as_millis() as u64),
            ] {
                assert_eq!(
                    frame(now, real, false),
                    frame_with_preview(now, real, preview, false)
                );
                assert_eq!(
                    frame(now, real, false),
                    frame_with_preview(now, real, Preview::default(), false)
                );
            }
        }
        // An invalid future preview also cannot suppress genuine activity.
        preview.set_enabled(true, ms(1000));
        assert_eq!(
            frame(ms(500), burst(0, 500), false),
            frame_with_preview(ms(500), burst(0, 500), preview, false)
        );
    }

    #[test]
    fn preview_off_crossfades_without_snapping_to_a_new_real_burst() {
        let mut preview = Preview::default();
        preview.set_enabled(true, ms(1000));
        let real = burst(7100, 7100);
        let before = frame_with_preview(ms(7100), real, preview, false);
        preview.set_enabled(false, ms(7100));
        assert_eq!(before, frame_with_preview(ms(7100), real, preview, false));
        let after = frame_with_preview(ms(7101), burst(7100, 7101), preview, false);
        assert!(before.points.iter().zip(&after.points).all(|(a,b)| (a.y-b.y).abs() < 0.02));
        for now in 7100..7700 {
            assert!(frame_with_preview(ms(now), burst(7100, now), preview, false).animating);
        }
        assert_eq!(
            frame(ms(7700), burst(7100, 7700), false),
            frame_with_preview(ms(7700), burst(7100, 7700), preview, false),
        );
    }

    #[test]
    fn measured_250ms_pulses_hold_without_extending_unchanged_activity() {
        for now in 250..2000 {
            let measured_at = now / 250 * 250;
            assert_eq!(envelope(ms(now), burst(0, measured_at), false), (1.0, true));
        }
        assert!(envelope(ms(2599), burst(0, 2000), false).1);
        assert_eq!(frame(ms(2600), burst(0, 2000), false), frame(ms(0), None, false));
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
}
