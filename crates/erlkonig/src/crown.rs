//! Erlkönig's wavelet crown, still at rest and gently moving during byte transfer.
//!
//! The outline is the canonical website's Gabor/Morlet crown, uniformly fitted
//! to a 36 px template image. Only the inner wave moves; walls, tips and base
//! stay put. There is no clock, timer, worker or network dependency here.
//!
//! The caller owns a monotonic origin and updates `last_transfer_at` ONLY when
//! received/sent byte counters increase, not when a peer connects or a poll
//! succeeds. Retain `began_at` throughout a burst; start a new burst after
//! `SETTLE_AFTER` without bytes. Keep the last burst until it settles: passing
//! `None` intentionally returns immediately to the exact resting outline.

use std::f64::consts::{PI, TAU};
use std::time::Duration;

pub const SIZE: u32 = 36;
/// After this much time without bytes the exact resting crown is restored.
pub const SETTLE_AFTER: Duration = Duration::from_millis(600);
const ATTACK: f64 = 0.160;
const HOLD: f64 = 0.120;
const RELEASE: f64 = 0.480;
const WAVE_STEPS: usize = 140;
const SCALE: f64 = 0.080;
const MAX_DISPLACEMENT: f64 = 0.85; // physical pixels, less than half a point at 2x

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TransferActivity {
    pub began_at: Duration,
    pub last_transfer_at: Duration,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Point {
    pub x: f64,
    pub y: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Frame {
    /// Closed polygon in the 36 × 36 template-image coordinate system.
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
    // Center the canonical 400 × 165.75 outline, preserving its proportions.
    Point {
        x: 18.0 + (x - 250.0) * SCALE,
        y: 18.0 + (y - 199.125) * SCALE,
    }
}

/// Pure monotonic time + observed activity → vector frame.
///
/// Reduced motion is exactly static, including while bytes are transferring.
/// Idle, invalid and fully settled inputs all produce bit-identical points.
pub fn frame(now: Duration, activity: Option<TransferActivity>, reduced_motion: bool) -> Frame {
    let (amount, animating) = envelope(now, activity, reduced_motion);
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
    // The two carrier periods divide six seconds. Reducing before converting to
    // f64 retains subsecond precision even after arbitrarily long app uptime.
    let time = (now.as_secs() % 6) as f64 + f64::from(now.subsec_nanos()) / 1e9;
    for i in 0..=WAVE_STEPS {
        let u = i as f64 / WAVE_STEPS as f64;
        let t = -catch + 2.0 * catch * u;
        let mut point = template_point(250.0 + 225.0 * t, wave_y(t));
        if amount > 0.0 && i != 0 && i != WAVE_STEPS {
            // A smooth compact envelope fixes both bridge endpoints AND their
            // tangents. Two gentle coherent carriers, no random jitter, no
            // flattening of the recognizable resting wavelet.
            let spatial = (PI * u).sin().powi(2);
            let carrier = 0.7 * (TAU * (2.0 * u - time / 1.2)).sin()
                + 0.3 * (TAU * (3.0 * u + time / 2.0)).sin();
            point.y += MAX_DISPLACEMENT * amount * spatial * carrier;
        }
        points.push(point);
    }
    points.push(template_point(450.0, apex_y));
    points.push(template_point(425.0, 282.0));
    points.push(points[0]);
    Frame { points, animating }
}

impl Frame {
    /// Black RGBA8 with coverage in alpha only, suitable for a macOS template.
    /// Four-by-four scanline supersampling needs no image/font/rendering stack.
    pub fn rgba(&self) -> (Vec<u8>, u32, u32) {
        const SS: usize = 4;
        let size = SIZE as usize;
        let mut coverage = vec![0u8; size * size];
        let mut crossings = Vec::with_capacity(self.points.len());
        for sy in 0..size * SS {
            let y = (sy as f64 + 0.5) / SS as f64;
            crossings.clear();
            for edge in self.points.windows(2) {
                let [a, b] = [edge[0], edge[1]];
                if (a.y > y) != (b.y > y) {
                    crossings.push(a.x + (y - a.y) * (b.x - a.x) / (b.y - a.y));
                }
            }
            crossings.sort_by(f64::total_cmp);
            for pair in crossings.chunks_exact(2) {
                let sample_index =
                    |x: f64| (x * SS as f64 - 0.5).ceil().clamp(0.0, (size * SS) as f64) as usize;
                for sx in sample_index(pair[0])..sample_index(pair[1]) {
                    coverage[(sy / SS) * size + sx / SS] += 1;
                }
            }
        }
        let mut rgba = vec![0; size * size * 4];
        for (pixel, samples) in rgba.chunks_exact_mut(4).zip(coverage) {
            pixel[3] = ((u16::from(samples) * 255 + 8) / 16) as u8;
        }
        (rgba, SIZE, SIZE)
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
        assert!(
            almost
                .points
                .iter()
                .zip(&idle.points)
                .all(|(a, b)| (a.y - b.y).abs() < 0.00002)
        );
        assert_eq!(idle, frame(ms(1760), burst(1000, 1160), false));
        assert_eq!(idle, frame(ms(2000), burst(1000, 1160), false));
    }

    #[test]
    fn coordinates_are_finite_bounded_and_outer_silhouette_does_not_move() {
        let idle = frame(ms(0), None, false);
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
                assert!((2.0..=34.0).contains(&p.x));
                assert!((10.0..=25.0).contains(&p.y));
                assert_eq!(p.x, idle.points[i].x);
                assert!((p.y - idle.points[i].y).abs() <= MAX_DISPLACEMENT);
                if i <= 2 || i >= WAVE_STEPS + 2 {
                    assert_eq!(*p, idle.points[i]);
                }
            }
        }
    }

    #[test]
    fn deterministic_inputs_and_long_uptime_keep_the_same_carrier() {
        let early = frame(ms(2234), burst(0, 2234), false);
        assert_eq!(early, frame(ms(2234), burst(0, 2234), false));
        let late = Duration::new(u64::MAX - u64::MAX % 6 + 2, 234_000_000);
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
    fn template_pixels_are_monochrome_and_do_not_touch_edges() {
        for active in [None, burst(0, 1000)] {
            let (pixels, w, h) = frame(ms(1000), active, false).rgba();
            assert_eq!((w, h), (36, 36));
            assert_eq!(pixels.len(), 36 * 36 * 4);
            assert!(pixels.chunks_exact(4).all(|p| p[..3] == [0, 0, 0]));
            assert!(pixels.chunks_exact(4).any(|p| p[3] == 255));
            assert!(pixels.chunks_exact(4).any(|p| p[3] > 0 && p[3] < 255));
            for y in 0..36 {
                for x in 0..36 {
                    if x == 0 || x == 35 || y == 0 || y == 35 {
                        assert_eq!(pixels[(y * 36 + x) * 4 + 3], 0);
                    }
                }
            }
        }
    }
}
