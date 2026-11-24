//! Fit the spatial Radial evaluator with Hermite cubics in normalized angular time.

use super::RadialDistribution;
use crate::{EngineError, Result};
use anicraft_doc::motion::MAX_KEYS;
use anicraft_geom::walk::CurveWalk;
use anicraft_geom::{Path, PathCmd, Vec2};

const GEOMETRY_TOLERANCE: f64 = 0.01;
const MAX_SEGMENTS: u32 = 256;

pub(super) fn path(radial: RadialDistribution) -> Result<Path> {
    let radians = radial.travel(1.0).to_radians();
    let turns = radial.travel(1.0).abs() / 360.0;
    let start_radius = f64::from(radial.radius);
    let raw_rate = turns * f64::from(radial.spiral_offset);
    if start_radius > 0.0 && start_radius + raw_rate < 0.0 {
        return Err(EngineError::Param("Spiral Timeline Animation cannot reach zero radius before End. Increase Radius or reduce the inward Spiral Offset.".into()));
    }
    let radius_rate = if start_radius == 0.0 { raw_rate.max(0.0) } else { raw_rate };
    let [dx, dy] = radial.centre_drift.map(|v| f64::from(v) * turns);

    // The fourth derivative is bounded by r*|w|^4 + 4*|r'|*|w|^3. Centre Drift
    // is linear. Cubic Hermite interpolation has error at most this bound*h^4/384.
    let w = radians.abs();
    let max_radius = start_radius.max(radial.radius_at(1.0));
    let bound = max_radius * w.powi(4) + 4.0 * radius_rate.abs() * w.powi(3);
    let needed = (bound / (384.0 * GEOMETRY_TOLERANCE)).sqrt().sqrt().ceil();
    if needed > f64::from(MAX_SEGMENTS) {
        return Err(EngineError::Param("The Spiral exceeds the native path approximation range.".into()));
    }
    let count = (needed as u32).max(1).next_power_of_two();
    let sample = |time: f64| {
        let theta = f64::from(radial.start) + radial.travel(time) % 360.0;
        let (sin, cos) = theta.to_radians().sin_cos();
        let r = radial.radius_at(time);
        let d = [dx + radius_rate * cos - r * radians * sin, dy + radius_rate * sin + r * radians * cos];
        (radial.point_f64(time), d)
    };
    let handle = |(p, d): ([f64; 2], [f64; 2]), sign: f64| {
        let k = sign / (3.0 * f64::from(count));
        Vec2::new((p[0] + d[0] * k) as f32, (p[1] + d[1] * k) as f32)
    };
    let mut cmds = vec![PathCmd::MoveTo(radial.point(0.0))];
    let mut from = sample(0.0);
    for i in 1..=count {
        let time = f64::from(i) / f64::from(count);
        let to = sample(time);
        cmds.push(PathCmd::CubicTo(handle(from, 1.0), handle(to, -1.0), radial.point(time)));
        from = to;
    }
    Ok(Path { cmds })
}

/// Express angular time as distance by inverting the native walk's measurements.
/// Simplify the piecewise-linear table within an arc-distance error, independent
/// of the eventual number of timeline frames.
pub(super) fn distance_map(path: &Path) -> Option<Vec<(f32, f32)>> {
    let walk = CurveWalk::new(path)?;
    let segments = path.cmds.len().checked_sub(1)? as f32;
    let length = walk.length();
    let samples: Vec<_> = walk.distance_samples()
        .map(|(cmd, t, distance)| (((cmd.saturating_sub(1)) as f32 + t) / segments, distance))
        .collect();
    let mut map = vec![(0.0, 0.0)];
    let mut intervals = vec![(0, samples.len().checked_sub(1)?)];
    let tolerance = 0.01_f32.max(length * f32::EPSILON);
    while let Some((a, b)) = intervals.pop() {
        let (&(t0, s0), &(t1, s1)) = (samples.get(a)?, samples.get(b)?);
        let mut worst = tolerance;
        let mut split = None;
        for (i, &(t, s)) in samples.get(a + 1..b)?.iter().enumerate() {
            let approx = s0 + (s1 - s0) * ((t - t0) / (t1 - t0));
            let error = (s - approx).abs();
            if error > worst {
                worst = error;
                split = Some(a + 1 + i);
            }
        }
        if let Some(mid) = split {
            intervals.push((mid, b));
            intervals.push((a, mid));
        } else {
            map.push((t1, s1 / length));
            if map.len() > MAX_KEYS {
                return None;
            }
        }
    }
    Some(map)
}
