//! Native cubic geometry for a signed Circle sweep.

mod properties;

use super::super::{ArrayEaseMode, ArrayEasing, ArrayPropertyStop, CLOSED_SWEEP_LIMIT, RadialDistribution, keep_pivot, posed_element};
use crate::{EngineError, Result};
use anicraft_doc::{Element, Keyframe, MotionTween, Props};
use anicraft_geom::{Path, PathCmd, Vec2};
use std::sync::Arc;

/// At most 30 degrees per cubic. With tangent handles of length
/// (4/3) * tan(step/4) * radius, radial error is below 4e-7 times the
/// radius before the motion path's f32 coordinate rounding.
fn path(radial: RadialDistribution) -> Path {
    let segments = (radial.sweep.abs() / 30.0).ceil().max(1.0) as u32;
    let step = f64::from(radial.sweep) / f64::from(segments);
    let handle = (4.0 / 3.0) * (step.to_radians() / 4.0).tan();
    let control = |i: u32, handle: f64| {
        // Reduce travel as in the shared Circle geometry, including at the seam.
        let angle = f64::from(radial.start) + (step * f64::from(i)) % 360.0;
        let (sin, cos) = angle.to_radians().sin_cos();
        let [cx, cy] = radial.centre.map(f64::from);
        let r = f64::from(radial.radius);
        Vec2::new((cx + r * (cos - handle * sin)) as f32, (cy + r * (sin + handle * cos)) as f32)
    };
    let mut cmds = vec![PathCmd::MoveTo(radial.point(0.0))];
    for i in 0..segments {
        let end = radial.point(f64::from(i + 1) / f64::from(segments));
        cmds.push(PathCmd::CubicTo(control(i, handle), control(i + 1, -handle), end));
    }
    Path { cmds }
}

pub(super) fn keyframe(
    source: &Keyframe,
    original: &Element,
    radial: RadialDistribution,
    seed: &ArrayPropertyStop,
    stops: &[ArrayPropertyStop],
    easing: &ArrayEasing,
    duration: u32,
) -> Result<Keyframe> {
    if radial.sweep.abs() > CLOSED_SWEEP_LIMIT {
        return Err(EngineError::Param("Circle Timeline Animation supports sweeps of up to one revolution (360 degrees).".into()));
    }
    if radial.radius == 0.0 || radial.sweep == 0.0 {
        return Err(EngineError::Param("Circle Timeline Animation needs a positive radius and a nonzero sweep.".into()));
    }
    let basis = original.transform.decompose();
    let pose = |stop: &ArrayPropertyStop| {
        let mut base = original.transform;
        let at = radial.point(0.0);
        base.m[4] = at.x;
        base.m[5] = at.y;
        let adjusted = stop.source_transform(original.transform, basis);
        let transform = keep_pivot(base, adjusted, original.pivot());
        posed_element(original, transform, Some(stop.clone()), Some(seed), false)
    };
    let element = pose(stops.first().unwrap_or(seed));
    if !element.transform.is_finite() {
        return Err(EngineError::Param("Circle Timeline Animation needs a finite transform.".into()));
    }

    let base = Props::of(&element);
    let mut motion = MotionTween::new(element.id);
    let last = duration - 1;
    // X/Y keys anchor the original path. Distribution timing uses normalized
    // authored stop positions, independent of rounding the property keys.
    if !motion.set_path(&path(radial), &base, last) {
        return Err(EngineError::Param("The Circle is too small or its control points exceed the native motion path range.".into()));
    }
    if easing.eases_distribution(stops) {
        let segments = match easing.mode {
            ArrayEaseMode::Global => vec![(0.0, easing.preset.clone())],
            ArrayEaseMode::PerSegment => super::property_times(stops)
                .into_iter()
                .map(|time| time as f32)
                .take_while(|&time| time < 1.0)
                .map(|time| {
                    let next = stops.partition_point(|stop| stop.offset <= time);
                    (time, easing.distribution_segment(stops, next).clone())
                })
                .collect(),
        };
        let turn = if radial.rotate { radial.sweep } else { 0.0 };
        if !motion.set_path_timing(segments, turn) {
            return Err(EngineError::Param("Circle Distribution needs monotonic easing. Back, Bounce and Elastic overshoot or reverse along the path.".into()));
        }
    }
    properties::write(&mut motion, &element, radial, stops, easing, source.start..source.start + duration, pose)?;
    let mut key = source.clone();
    key.duration = duration;
    key.elements = vec![Arc::new(element)];
    key.motion = Some(motion);
    Ok(key)
}
