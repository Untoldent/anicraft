//! Baked symbol arrays and the editor's reversible preview of the same command.
//!
//! A count of N produces N total instances, including the original (ID preserved).
//! The first instance moves to the requested start location. Copies are ordinary
//! symbol instances, not a new document object, so existing editors and exports work.
//! Geometry edits that only move instances reuse the existing preview.

use crate::params::{opt_bool, opt_f32, opt_str, opt_u32};
use crate::{CommandSpec, EngineError, Result, Session};
use anicraft_doc::{Element, ElementId, ElementKind};
use anicraft_geom::{Affine, Vec2};
use serde_json::{Value, json};
use std::ops::Range;
use std::sync::Arc;

const MAX_COPIES: u32 = 500;
pub const CLOSED_SWEEP_LIMIT: f32 = 360.0;

/// The same continuous curve used for Array placement and the editor's stage guide.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RadialDistribution {
    pub centre: [f32; 2],
    pub radius: f32,
    pub start: f32,
    pub sweep: f32,
    pub closed_loop: bool,
    pub spiral_offset: f32,
    pub centre_drift: [f32; 2],
    pub rotate: bool,
}

impl RadialDistribution {
    pub fn sweep_limit(self) -> f32 {
        if self.closed_loop { CLOSED_SWEEP_LIMIT } else { f32::INFINITY }
    }

    fn travel(self, fraction: f64) -> f64 {
        let limit = self.sweep_limit();
        f64::from(self.sweep.clamp(-limit, limit)) * fraction
    }

    pub fn radius_at(self, fraction: f64) -> f64 {
        let turns = self.travel(fraction).abs() / f64::from(CLOSED_SWEEP_LIMIT);
        self.radius_after_turns(turns)
    }

    fn radius_after_turns(self, turns: f64) -> f64 {
        let offset = if self.closed_loop { 0.0 } else { f64::from(self.spiral_offset) };
        (f64::from(self.radius) + offset * turns).max(0.0)
    }

    /// Local centre after this many unsigned revolutions of an open distribution.
    pub fn centre_after_turns(self, turns: f64) -> [f64; 2] {
        let [x, y] = self.centre.map(f64::from);
        let [dx, dy] = if self.closed_loop { [0.0, 0.0] } else { self.centre_drift.map(f64::from) };
        [x + dx * turns, y + dy * turns]
    }

    pub fn point(self, fraction: f64) -> Vec2 {
        let travel = self.travel(fraction);
        let turns = travel.abs() / f64::from(CLOSED_SWEEP_LIMIT);
        // Reduce whole turns before adding the start angle so the seam is exact.
        let angle = f64::from(self.start) + travel % f64::from(CLOSED_SWEEP_LIMIT);
        let (sin, cos) = angle.to_radians().sin_cos();
        let r = self.radius_after_turns(turns);
        let [x, y] = self.centre_after_turns(turns);
        Vec2::new((x + r * cos) as f32, (y + r * sin) as f32)
    }
}

/// One property snapshot on the array's 0..1 progression.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ArrayPropertyStop {
    pub offset: f32,
    pub alpha: f32,
    pub scale: [f32; 2],
    pub rotation: f32,
}

impl ArrayPropertyStop {
    pub const LIMIT: usize = 15;
    pub const SCALE_LIMIT: f32 = 100.0;
    pub const ROTATION_LIMIT: f32 = 100_000.0;

    pub const fn new(offset: f32, alpha: f32) -> Self {
        Self { offset, alpha, scale: [1.0; 2], rotation: 0.0 }
    }

    /// Hold the endpoint value outside the first or last stop, as gradients do.
    /// Both the editor and command pass in stops sorted by position.
    pub fn sample(stops: impl IntoIterator<Item = Self>, t: f32) -> Self {
        let mut stops = stops.into_iter();
        let Some(mut a) = stops.next() else { return Self::new(t, 1.0) };
        if t <= a.offset { return Self { offset: t, ..a }; }
        for b in stops {
            if t <= b.offset {
                let span = b.offset - a.offset;
                if span <= f32::EPSILON || t == b.offset { return Self { offset: t, ..b }; }
                let f = ((t - a.offset) / span).clamp(0.0, 1.0);
                let blend = |a, b| a + (b - a) * f;
                return Self {
                    offset: t,
                    alpha: blend(a.alpha, b.alpha),
                    scale: [blend(a.scale[0], b.scale[0]), blend(a.scale[1], b.scale[1])],
                    rotation: blend(a.rotation, b.rotation),
                };
            }
            a = b;
        }
        Self { offset: t, ..a }
    }

    fn apply_transform(self, base: Affine, pivot: Option<Vec2>) -> Affine {
        let [sx, sy] = self.scale;
        // Wrap after interpolation so full turns affect the intermediate copies.
        let degrees = self.rotation % CLOSED_SWEEP_LIMIT;
        if [sx, sy] == [1.0; 2] && degrees == 0.0 { return base; }
        let mut adjusted = Affine::rotate(degrees.to_radians()).mul(&base.mul(&Affine::scale(sx, sy)));
        let shift = pivot.map_or(Vec2::ZERO, |p| base.apply_vector(p) - adjusted.apply_vector(p));
        adjusted.m[4] = base.m[4] + shift.x;
        adjusted.m[5] = base.m[5] + shift.y;
        adjusted
    }
}

impl Session {
    pub fn array_preview_active(&self) -> bool {
        self.array_preview
    }

    /// Shared by geometry-only previews with the same instances, source artwork and stacking.
    pub fn array_translation_generation(&self) -> Option<crate::StateId> {
        self.array_positions.as_ref().map(|preview| preview.generation)
    }

    pub(super) fn require_no_array_preview(&self) -> Result<()> {
        if self.array_preview {
            return Err(EngineError::State("commit or cancel the Array preview first".into()));
        }
        Ok(())
    }

    /// Start a preview from the original artwork. Reuse the instances for position-only edits.
    pub fn preview_array(&mut self, params: &Value) -> Result<()> {
        if self.array_preview {
            if let Some(mut preview) = self.array_positions.take()
                && preview.move_to(self, params)
            {
                self.array_positions = Some(preview);
                return Ok(());
            }
            self.restore_gesture();
        } else {
            if self.in_gesture() || self.live_cut().is_some() {
                return Err(EngineError::State("finish the current edit before starting an array".into()));
            }
            self.begin_gesture("Array");
        }
        let result = self.execute_command("array.create", params).map(|_| ());
        if result.is_ok() {
            self.array_preview = true;
            self.array_positions = PositionPreview::new(self, params);
        } else if !self.array_preview {
            self.end_gesture();
        }
        result
    }

    pub fn finish_array_preview(&mut self, commit: bool) {
        if !std::mem::take(&mut self.array_preview) {
            return;
        }
        let preview = self.array_positions.take();
        if !commit {
            self.revert_gesture();
        } else if let Some(preview) = preview {
            // Record the current parameters once, including moves made without replaying the
            // command. Replaying history still uses the public Array command.
            self.rec.truncate(self.gesture_rec);
            self.rec.push(crate::history::Recorded::new("array.create", &preview.params));
        }
        self.end_gesture();
    }
}

/// An existing stretch of baked preview instances whose topology stays unchanged on a move.
/// Tweens with keyed motion and linked layers keep the general command path.
pub(super) struct PositionPreview {
    generation: crate::StateId,
    key: usize,
    instances: Range<usize>,
    reversed: bool,
    params: Value,
}

const LINEAR_COORDS: [&str; 4] = ["start_x", "start_y", "dx", "dy"];
const RADIAL_COORDS: [&str; 8] = ["center_x", "center_y", "radius", "start_angle", "sweep_angle", "spiral_offset", "drift_x", "drift_y"];

fn explicit_positions(p: &Value) -> Option<(Layout, &'static [&'static str])> {
    match p.get("mode")?.as_str()? {
        "linear" => {
            let [start_x, start_y, dx, dy] = LINEAR_COORDS.map(|key| opt_f32(p, key).ok().flatten());
            Some((Layout::Linear { start_x: start_x?, start_y: start_y?, dx: dx?, dy: dy? }, &LINEAR_COORDS))
        }
        "radial" if ["radius", "start_angle"].iter().all(|key| p.get(*key).is_some_and(Value::is_number)) => {
            let radial = parse_radial(p, Vec2::ZERO).ok()?;
            (!radial.rotate).then_some((Layout::Radial(radial), &RADIAL_COORDS))
        }
        _ => None,
    }
}

impl PositionPreview {
    fn new(s: &Session, p: &Value) -> Option<Self> {
        explicit_positions(p)?;
        let tl = s.timeline();
        if tl.has_rig() { return None; }
        let layer = tl.layer(s.layer)?;
        let key = layer.keyframe_index_at(s.frame)?;
        let kf = layer.keyframes.get(key)?;
        if kf.motion.is_some() { return None; }
        let count = opt_u32(p, "count").ok()?.unwrap_or(10) as usize;
        let first = kf.elements.iter().position(|e| s.selection.contains(s.layer, e.id))?;
        let instances = first..first + count;
        let reversed = p.get("stacking").and_then(Value::as_str) == Some("front_to_back");
        let copies = kf.elements.get(instances.clone())?;
        // A transformed custom pivot can offset the copy from its layout position.
        // Replay from the source so those offsets stay exact after a move.
        if copies.first()?.pivot().is_some() && p.get("property_stops").is_some()
            && parse_stops(p).ok()?.is_some_and(|stops| stops.iter().any(|stop| stop.scale != [1.0; 2] || stop.rotation != 0.0))
        {
            return None;
        }
        let ids = copies.iter().map(|e| e.id);
        let in_order = if reversed { ids.rev().eq(s.selection.ids()) } else { ids.eq(s.selection.ids()) };
        in_order.then(|| Self { generation: crate::history::fresh_state(), key, instances, reversed, params: p.clone() })
    }

    fn move_to(&mut self, s: &mut Session, params: &Value) -> bool {
        let (Some(old), Some(new), Some((layout, coords))) = (self.params.as_object(), params.as_object(), explicit_positions(params)) else { return false };
        if old.len() != new.len() || old.iter().any(|(key, v)| !coords.contains(&key.as_str()) && new.get(key) != Some(v)) {
            return false;
        }
        if old == new { return true; }
        let count = self.instances.len() as u32;
        // Linear coordinates and nonrotating radial positions stay within finite bounds
        // between their endpoints. The explicit inputs came through opt_f32.
        if ![0, count.saturating_sub(1)].into_iter().all(|i| layout.place(Affine::IDENTITY, i, count).0.is_finite()) {
            return false;
        }
        let Some(instances) = s.layer_mut(None).ok().and_then(|layer| layer.keyframe_mut(self.key))
            .and_then(|kf| kf.elements.get_mut(self.instances.clone())) else { return false };
        let mut changed = false;
        for (i, el) in instances.iter_mut().enumerate() {
            let i = if self.reversed { count - 1 - i as u32 } else { i as u32 };
            let (xf, _) = layout.place(el.transform, i, count);
            if xf != el.transform {
                Arc::make_mut(el).transform = xf;
                changed = true;
            }
        }
        self.params = params.clone();
        if changed {
            s.state = crate::history::fresh_state();
            s.revision += 1;
            s.dl_cache.replace(None);
        }
        true
    }
}

pub fn specs() -> Vec<CommandSpec> {
    vec![CommandSpec {
        id: "array.create",
        label: "Create Array",
        menu: "",
        shortcut: None,
        params: "{mode: linear|radial, count: 1..500 (includes original), dx?, dy?, start_x?, start_y? (linear), center_x?, center_y?, radius?, start_angle?, sweep_angle?, closed_loop?, spiral_offset?, drift_x?, drift_y?, rotate? (radial), stacking?: back_to_front|front_to_back, property_stops?: [{offset: 0..1, alpha?: 0..1, scale_x?: -100..100, scale_y?: -100..100, rotation?: -100000..100000}, ...] (2..15), alpha_stops?: [{offset: 0..1, alpha: 0..1}, ...] (2..15, legacy alternative)}. Scale is a local multiplier (default 1), rotation is an additive angle in degrees (default 0), and Alpha is a multiplier (default 1). Radial sweep defaults to 360 degrees. Closed loop defaults to true, uses count intervals and limits sweep to one turn. Open mode uses count minus one intervals. spiral_offset changes radius down to zero and drift_x/drift_y move the centre. All three rates default to zero and are measured in pixels per turn. Requires exactly one selected symbol instance on the active layer. Moves the original to position 1, bakes the copies and selects them all.",
        mutates: true,
        run: create,
    }]
}

fn parse_stops(p: &Value) -> Result<Option<Vec<ArrayPropertyStop>>> {
    let (field, value, legacy) = match (p.get("property_stops"), p.get("alpha_stops")) {
        (Some(_), Some(_)) => return Err(EngineError::Param("use property_stops or alpha_stops".into())),
        (Some(v), None) => ("property_stops", v, false),
        (None, Some(v)) => ("alpha_stops", v, true),
        (None, None) => return Ok(None),
    };
    let stops = value.as_array().ok_or_else(|| EngineError::Param(format!("{field} must be an array")))?;
    if !(2..=ArrayPropertyStop::LIMIT).contains(&stops.len()) {
        return Err(EngineError::Param(format!("{field} needs 2..={} stops", ArrayPropertyStop::LIMIT)));
    }
    let mut out = Vec::with_capacity(stops.len());
    for v in stops {
        let number = |key: &str, min: f32, max: f32, default: Option<f32>| -> Result<f32> {
            if v.get(key).is_none() && let Some(default) = default { return Ok(default); }
            let n = v.get(key).and_then(Value::as_f64).ok_or_else(|| EngineError::Param(format!("{field} entries need numeric '{key}'")))?;
            if !(f64::from(min)..=f64::from(max)).contains(&n) {
                return Err(EngineError::Param(format!("{field} '{key}' must be in {min}..={max}")));
            }
            Ok(n as f32)
        };
        let offset = number("offset", 0.0, 1.0, None)?;
        let alpha = number("alpha", 0.0, 1.0, (!legacy).then_some(1.0))?;
        let mut stop = ArrayPropertyStop::new(offset, alpha);
        if !legacy {
            let scale = ArrayPropertyStop::SCALE_LIMIT;
            let rotation = ArrayPropertyStop::ROTATION_LIMIT;
            stop.scale = [number("scale_x", -scale, scale, Some(1.0))?, number("scale_y", -scale, scale, Some(1.0))?];
            stop.rotation = number("rotation", -rotation, rotation, Some(0.0))?;
        }
        out.push(stop);
    }
    out.sort_by(|a, b| a.offset.total_cmp(&b.offset));
    Ok(Some(out))
}

#[derive(Clone, Copy, Debug)]
enum Layout {
    Linear { start_x: f32, start_y: f32, dx: f32, dy: f32 },
    Radial(RadialDistribution),
}

impl Layout {
    fn place(self, source: Affine, index: u32, count: u32) -> (Affine, f32) {
        match self {
            Layout::Linear { start_x, start_y, dx, dy } => {
                let mut m = source;
                m.m[4] = start_x + index as f32 * dx;
                m.m[5] = start_y + index as f32 * dy;
                let t = if count <= 1 { 0.0 } else { index as f32 / (count - 1) as f32 };
                (m, t)
            }
            Layout::Radial(radial) => {
                let divisor = count.saturating_sub(u32::from(!radial.closed_loop)).max(1);
                let t = f64::from(index) / f64::from(divisor);
                // Rotate the basis by the signed travel, then set the registration point.
                let mut m = if radial.rotate { Affine::rotate(radial.travel(t).to_radians() as f32).mul(&source) } else { source };
                let at = radial.point(t);
                m.m[4] = at.x;
                m.m[5] = at.y;
                (m, t as f32)
            }
        }
    }
}

fn parse_radial(p: &Value, original: Vec2) -> Result<RadialDistribution> {
    let x = opt_f32(p, "center_x")?.ok_or_else(|| EngineError::Param("radial requires center_x".into()))?;
    let y = opt_f32(p, "center_y")?.ok_or_else(|| EngineError::Param("radial requires center_y".into()))?;
    let delta = original - Vec2::new(x, y);
    let radius = opt_f32(p, "radius")?.unwrap_or_else(|| delta.x.hypot(delta.y));
    if !radius.is_finite() || radius < 0.0 {
        return Err(EngineError::Param("radius must be finite and >= 0".into()));
    }
    Ok(RadialDistribution {
        centre: [x, y], radius,
        start: opt_f32(p, "start_angle")?.unwrap_or_else(|| delta.y.atan2(delta.x).to_degrees()),
        sweep: opt_f32(p, "sweep_angle")?.unwrap_or(CLOSED_SWEEP_LIMIT),
        closed_loop: opt_bool(p, "closed_loop")?.unwrap_or(true),
        spiral_offset: opt_f32(p, "spiral_offset")?.unwrap_or(0.0),
        centre_drift: [opt_f32(p, "drift_x")?.unwrap_or(0.0), opt_f32(p, "drift_y")?.unwrap_or(0.0)],
        rotate: opt_bool(p, "rotate")?.unwrap_or(false),
    })
}

fn create(s: &mut Session, p: &Value) -> Result<Value> {
    let count = opt_u32(p, "count")?.unwrap_or(10);
    if !(1..=MAX_COPIES).contains(&count) {
        return Err(EngineError::Param(format!("count must be in 1..={MAX_COPIES} (including original)")));
    }
    let mode = opt_str(p, "mode")?.unwrap_or("linear");
    let front_to_back = match opt_str(p, "stacking")? {
        None | Some("back_to_front") => false,
        Some("front_to_back") => true,
        Some(other) => return Err(EngineError::Param(format!("unknown stacking '{other}'"))),
    };
    let stops = parse_stops(p)?;
    let Some((_, id)) = s.selection.first().filter(|(layer, _)| *layer == s.layer && s.selection.len() == 1) else {
        return Err(EngineError::State("select exactly one symbol instance on the active layer".into()));
    };
    let original = s.editable_keyframe()?.elements.iter().find(|e| e.id == id)
        .map(|e| (**e).clone())
        .ok_or_else(|| EngineError::State("selected instance is not in the editable keyframe".into()))?;
    if !matches!(&original.kind, ElementKind::Symbol { .. }) {
        return Err(EngineError::State("the initial Array implementation requires a symbol instance".into()));
    }
    let [original_x, original_y] = [original.transform.m[4], original.transform.m[5]];
    let layout = match mode {
        "linear" => Layout::Linear {
            start_x: opt_f32(p, "start_x")?.unwrap_or(original_x),
            start_y: opt_f32(p, "start_y")?.unwrap_or(original_y),
            dx: opt_f32(p, "dx")?.unwrap_or(30.0),
            dy: opt_f32(p, "dy")?.unwrap_or(0.0),
        },
        "radial" => Layout::Radial(parse_radial(p, original.transform.translation())?),
        other => return Err(EngineError::Param(format!("unknown array mode '{other}' (linear, radial)"))),
    };

    let mut copies: Vec<Arc<Element>> = Vec::with_capacity(count as usize);
    let mut ids: Vec<ElementId> = Vec::with_capacity(count as usize);
    for i in 0..count {
        let (base, t) = layout.place(original.transform, i, count);
        let stop = stops.as_deref().map(|stops| ArrayPropertyStop::sample(stops.iter().copied(), t));
        let transform = stop.map_or(base, |stop| stop.apply_transform(base, original.pivot()));
        if !transform.is_finite() {
            return Err(EngineError::Param("array positions exceeded finite coordinate range".into()));
        }
        let mut el = original.clone();
        // Instance #1 is the original and keeps its ID.
        if i > 0 { el.id = ElementId(s.doc.alloc_id()); }
        el.transform = transform;
        if let Some(stop) = stop {
            el.color = el.color.with_alpha(stop.alpha);
        }
        ids.push(el.id);
        copies.push(Arc::new(el));
    }

    // Preserve the source's position relative to unrelated artwork. Copies stack beside it.
    // Only the relative order among the array copies is controlled by "stacking".
    if front_to_back { copies.reverse(); }
    let kf = s.editable_keyframe()?;
    let at = kf.elements.iter().position(|e| e.id == id)
        .ok_or_else(|| EngineError::State("original disappeared from the keyframe".into()))?;
    kf.elements.remove(at);
    kf.elements.splice(at..at, copies);
    s.select_only(ids.iter().copied());
    Ok(json!({"count": count, "mode": mode, "instances": ids.iter().map(|id| id.0).collect::<Vec<_>>()}))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn st(p: f32, alpha: f32) -> ArrayPropertyStop { ArrayPropertyStop::new(p, alpha) }

    #[test]
    fn independent_stops_can_fade_out_then_back_in() {
        let stops = [st(0.0, 1.0), st(0.5, 0.0), st(1.0, 1.0)];
        assert!((ArrayPropertyStop::sample(stops, 0.25).alpha - 0.5).abs() < 1e-6);
        assert!((ArrayPropertyStop::sample(stops, 0.75).alpha - 0.5).abs() < 1e-6);
        assert_eq!(ArrayPropertyStop::sample(stops, 0.5).alpha, 0.0);
    }

    #[test]
    fn transform_stops_bake_relative_scale_and_two_full_turns_with_alpha() {
        let mut s = radial_source();
        let id = s.selection.ids().next().unwrap();
        let source = s.editable_keyframe().unwrap().elements.iter_mut().find(|e| e.id == id).unwrap();
        let source = Arc::make_mut(source);
        source.transform = Affine::rotate(30.0_f32.to_radians()).mul(&Affine::scale(2.0, 0.5));
        let alpha = anicraft_doc::ColorEffect::Alpha(0.8);
        source.color = alpha;
        s.execute("array.create", &json!({"count": 9, "start_x": 25, "start_y": 60, "dx": 40,
            "property_stops": [{"offset": 0, "alpha": 1}, {"offset": 1, "alpha": 0.25, "scale_x": 2, "scale_y": 0.5, "rotation": 720}]})).unwrap();
        assert_eq!(s.selection.len(), 9);
        for (i, (_, copy)) in s.selected_elements().into_iter().enumerate() {
            let t = i as f32 / 8.0;
            let (sin, cos) = (30.0 + i as f32 * 90.0).to_radians().sin_cos();
            let xf = copy.transform;
            assert_eq!(xf.translation(), Vec2::new(25.0 + i as f32 * 40.0, 60.0));
            assert!(xf.apply_vector(Vec2::new(1.0, 0.0)).distance(Vec2::new(cos, sin) * (2.0 * (1.0 + t))) < 1e-5);
            assert!(xf.apply_vector(Vec2::new(0.0, 1.0)).distance(Vec2::new(-sin, cos) * (0.5 * (1.0 - 0.5 * t))) < 1e-5);
            assert_eq!(copy.color, alpha.with_alpha(1.0 - 0.75 * t));
        }
    }

    #[test]
    fn transform_stops_follow_radial_progression_and_add_to_rotate_instances_about_the_pivot() {
        let mut s = radial_source();
        let id = s.selection.ids().next().unwrap();
        let source = s.editable_keyframe().unwrap().elements.iter_mut().find(|e| e.id == id).unwrap();
        let source = Arc::make_mut(source);
        let pivot = Vec2::new(4.0, -3.0);
        assert!(source.set_pivot(Some(pivot)));
        source.transform = Affine::rotate(15.0_f32.to_radians()).mul(&Affine::scale(1.5, 0.75));
        let basis = source.transform;
        for (closed_loop, rotate, sweep) in [(true, true, -360.0), (false, false, 630.0), (false, true, -630.0)] {
            let p = json!({"mode": "radial", "count": 5, "center_x": 210, "center_y": 80, "radius": 40, "start_angle": 12,
                "sweep_angle": sweep, "closed_loop": closed_loop, "rotate": rotate, "spiral_offset": 33, "drift_x": 24, "drift_y": -17,
                "property_stops": [{"offset": 0, "rotation": -45, "scale_x": 0.5}, {"offset": 1, "rotation": 675, "scale_x": 2.5, "scale_y": 2} ]});
            let radial = parse_radial(&p, Vec2::ZERO).unwrap();
            let copies = placed(&mut s, &p);
            assert_eq!(copies.len(), 5);
            for (i, xf) in copies.into_iter().enumerate() {
                let t = i as f64 / if closed_loop { 5.0 } else { 4.0 };
                let travel = if rotate { sweep * t } else { 0.0 };
                let base = Affine::rotate((travel as f32).to_radians()).mul(&basis);
                let anchor = radial.point(t) + base.apply_vector(pivot);
                assert!(xf.apply(pivot).distance(anchor) < 0.0001);
                let angle = (15.0 + travel - 45.0 + 720.0 * t).to_radians() as f32;
                let (sin, cos) = angle.sin_cos();
                let t = t as f32;
                assert!(xf.apply_vector(Vec2::new(1.0, 0.0)).distance(Vec2::new(cos, sin) * (1.5 * (0.5 + 2.0 * t))) < 1e-5);
                assert!(xf.apply_vector(Vec2::new(0.0, 1.0)).distance(Vec2::new(-sin, cos) * (0.75 * (1.0 + t))) < 1e-5);
            }
        }
        assert!(s.array_translation_generation().is_none());
        s.finish_array_preview(false);
    }

    fn radial_source() -> Session {
        let mut s = Session::default();
        s.execute("draw.rect", &json!({"x": 5, "y": 10, "w": 20, "h": 18})).unwrap();
        s.execute("modify.convert_to_symbol", &json!({"name": "Source"})).unwrap();
        s
    }

    fn placed(s: &mut Session, p: &Value) -> Vec<Affine> {
        s.preview_array(p).unwrap();
        s.selected_elements().into_iter().map(|(_, el)| el.transform).collect()
    }

    fn assert_polar(at: Affine, radius: f32, angle: f32) {
        let (sin, cos) = angle.to_radians().sin_cos();
        let expected = anicraft_geom::Vec2::new(radius * cos, radius * sin);
        assert!(at.translation().distance(expected) < 1e-3, "expected {expected:?}, got {:?}", at.translation());
    }

    #[test]
    fn closed_radial_defaults_to_one_turn_and_keeps_count_intervals_below_it() {
        let mut s = radial_source();
        let mut p = json!({"mode": "radial", "center_x": 0, "center_y": 0, "radius": 50, "start_angle": 0});
        for count in [1, 2, 4, 9] {
            p["count"] = json!(count);
            let copies = placed(&mut s, &p);
            assert_eq!(copies.len(), count);
            for (i, at) in copies.into_iter().enumerate() {
                assert_polar(at, 50.0, 360.0 * i as f32 / count as f32);
            }
        }
        // A spiral value remains available when switching back to open mode.
        p["count"] = json!(4);
        p["spiral_offset"] = json!(35);
        p["closed_loop"] = json!(true);
        for sweep in [358.0f32, 360.0, 362.0, -358.0, -360.0, -362.0, 720.0, -1080.0] {
            p["sweep_angle"] = json!(sweep);
            let copies = placed(&mut s, &p);
            assert_polar(copies[0], 50.0, 0.0);
            assert_polar(copies[3], 50.0, sweep.clamp(-360.0, 360.0) * 0.75);
        }
    }

    #[test]
    fn closed_radial_full_turn_ends_exactly_at_start_without_placing_a_seam_copy() {
        let mut s = radial_source();
        for sweep in [-360.0, 360.0] {
            let radial = RadialDistribution { centre: [0.0, 0.0], radius: 420.0, start: 0.0,
                sweep, closed_loop: true, spiral_offset: 0.0, centre_drift: [60.0, -10.0], rotate: true };
            assert_eq!(radial.point(0.0), radial.point(1.0));
            let p = json!({"mode": "radial", "center_x": 0, "center_y": 0, "radius": 420,
                "start_angle": 0, "sweep_angle": sweep, "count": 10, "closed_loop": true, "rotate": true,
                "drift_x": 60, "drift_y": -10});
            let copies = placed(&mut s, &p);
            assert_eq!(copies.len(), 10);
            assert_eq!(copies[0].translation(), radial.point(0.0));
            assert!(copies.iter().skip(1).all(|copy| copy.translation() != radial.point(0.0)));
        }
    }

    #[test]
    fn centre_drift_accumulates_independently_of_radius_and_sweep_direction() {
        let mut s = radial_source();
        for sweep in [-720.0_f32, 720.0] {
            for offset in [-40.0_f32, 0.0, 25.0] {
                let p = json!({"mode": "radial", "center_x": 10, "center_y": 15, "radius": 40,
                    "start_angle": 0, "sweep_angle": sweep, "count": 9, "closed_loop": false,
                    "spiral_offset": offset, "drift_x": 30, "drift_y": -12});
                let copies = placed(&mut s, &p);
                assert_eq!(copies.len(), 9);
                for (i, copy) in copies.into_iter().enumerate() {
                    let turns = i as f32 / 4.0;
                    let centre = Vec2::new(10.0 + 30.0 * turns, 15.0 - 12.0 * turns);
                    let radius = (40.0 + offset * turns).max(0.0);
                    let (sin, cos) = (sweep.signum() * turns * std::f32::consts::TAU).sin_cos();
                    assert!(copy.translation().distance(centre + Vec2::new(cos, sin) * radius) < 1e-4);
                }
            }
        }
    }

    #[test]
    fn open_radial_keeps_both_endpoints_through_positive_and_negative_turn_boundaries() {
        let mut s = radial_source();
        let mut p = json!({"mode": "radial", "closed_loop": false, "center_x": 0, "center_y": 0, "radius": 50, "start_angle": 23});
        for count in [1, 2, 5, 8, 17] {
            p["count"] = json!(count);
            for sweep in [358.0, 360.0, 362.0, -358.0, -360.0, -362.0, 720.0, 1080.0] {
                p["sweep_angle"] = json!(sweep);
                let copies = placed(&mut s, &p);
                assert_eq!(copies.len(), count);
                assert_polar(copies[0], 50.0, 23.0);
                let last = if count == 1 { 23.0 } else { 23.0 + sweep };
                assert_polar(*copies.last().unwrap(), 50.0, last);
                if count > 2 {
                    let middle = count / 2;
                    let fraction = middle as f32 / (count - 1) as f32;
                    assert_polar(copies[middle], 50.0, 23.0 + sweep * fraction);
                }
            }
        }
    }

    #[test]
    fn spiral_grows_or_shrinks_per_turn_in_both_directions_and_stops_at_the_centre() {
        let mut s = radial_source();
        let mut p = json!({"mode": "radial", "closed_loop": false, "center_x": 0, "center_y": 0,
            "radius": 40, "start_angle": 0, "count": 7});
        for sign in [-1.0, 1.0] {
            for turns in [2.0, 3.0] {
                for offset in [-20.0, 15.0] {
                    let sweep = sign * 360.0 * turns;
                    p["sweep_angle"] = json!(sweep);
                    p["spiral_offset"] = json!(offset);
                    for (i, at) in placed(&mut s, &p).into_iter().enumerate() {
                        let t = i as f32 / 6.0;
                        let expected = (40.0f32 + offset * turns * t).max(0.0);
                        assert!(at.is_finite());
                        assert_polar(at, expected, sweep * t);
                    }
                }
            }
        }
        p["spiral_offset"] = json!(-40);
        for sweep in [359.0f32, 360.0, 361.0] {
            p["sweep_angle"] = json!(sweep);
            let at = *placed(&mut s, &p).last().unwrap();
            assert_polar(at, (40.0 - 40.0 * sweep / 360.0).max(0.0), sweep);
        }
    }

    #[test]
    fn spiral_rotation_uses_signed_travel_without_moving_the_registration_point() {
        let mut s = radial_source();
        let mut p = json!({"mode": "radial", "closed_loop": false, "center_x": 0, "center_y": 0,
            "radius": 50, "start_angle": 0, "spiral_offset": 16, "count": 2});
        for sweep in [-810.0f32, 810.0] {
            p["sweep_angle"] = json!(sweep);
            p["rotate"] = json!(false);
            let fixed = placed(&mut s, &p)[1];
            p["rotate"] = json!(true);
            let turned = placed(&mut s, &p)[1];
            assert_eq!(turned.translation(), fixed.translation());
            assert!((turned.m[1] - sweep.signum()).abs() < 1e-5);
            assert!(turned.m[0].abs() < 1e-5);
        }
    }

    #[test]
    fn invalid_radial_topology_and_spiral_values_do_not_change_the_document() {
        let mut s = radial_source();
        let (doc, selection, steps) = (s.doc.clone(), s.selection.clone(), s.history_len());
        for (key, value) in [("closed_loop", json!("false")), ("spiral_offset", json!("ten")), ("spiral_offset", json!(1e30)),
            ("drift_x", json!("right")), ("drift_y", json!(1e30))] {
            let mut p = json!({"mode": "radial", "center_x": 0, "center_y": 0, "closed_loop": false});
            p[key] = value;
            assert!(s.execute("array.create", &p).is_err());
            assert!(s.doc == doc && s.selection == selection && s.history_len() == steps);
        }
    }

    #[test]
    fn nonrotating_spiral_edits_reuse_the_preview_and_commit_exactly_the_last_geometry() {
        let mut s = radial_source();
        let base = s.doc.clone();
        let source = s.selection.clone();
        let mut p = json!({"mode": "radial", "closed_loop": false, "center_x": 200, "center_y": 50,
            "radius": 38, "start_angle": 15, "sweep_angle": 358, "spiral_offset": 15, "count": 20, "drift_x": 0, "drift_y": 0,
            "stacking": "front_to_back", "alpha_stops": [{"offset": 0, "alpha": 1}, {"offset": 1, "alpha": 0.2}]});
        s.preview_array(&p).unwrap();
        let generation = s.array_translation_generation();
        assert!(generation.is_some());
        let ids = s.selection.clone();
        let first = s.selected_elements().last().unwrap().1.clone();
        p["sweep_angle"] = json!(721);
        p["spiral_offset"] = json!(-30);
        p["drift_x"] = json!(46);
        p["drift_y"] = json!(-20);
        s.preview_array(&p).unwrap();
        assert_eq!(s.array_translation_generation(), generation);
        assert_eq!(s.selection, ids);
        assert!(Arc::ptr_eq(&first, &s.selected_elements().last().unwrap().1));
        s.finish_array_preview(true);

        let mut expected = Session::new(base);
        expected.selection = source;
        expected.execute("array.create", &p).unwrap();
        assert!(s.doc == expected.doc && s.selection == expected.selection);
        let (_, command) = s.history.steps().last().unwrap();
        assert_eq!(command.unwrap(), &[crate::history::Recorded::new("array.create", &p)]);
    }

    #[test]
    fn linear_count_includes_original_and_hits_last_stop() {
        let l = Layout::Linear { start_x: 12.0, start_y: 18.0, dx: 10.0, dy: -2.0 };
        let (a, ta) = l.place(Affine::IDENTITY, 0, 10);
        let (b, tb) = l.place(Affine::IDENTITY, 9, 10);
        assert_eq!(a.translation().x, 12.0);
        assert_eq!(b.translation().x, 102.0);
        assert_eq!(ta, 0.0);
        assert_eq!(tb, 1.0);
    }

    #[test]
    fn command_creates_baked_instances_as_one_undoable_edit() {
        let mut s = Session::default();
        assert!(s.execute("draw.rect", &json!({"x": 10, "y": 10, "w": 24, "h": 14, "fill": "#ffcc00", "stroke": null})).is_ok());
        assert!(s.execute("modify.convert_to_symbol", &json!({"name": "Test petal"})).is_ok());
        assert_eq!(s.selection.len(), 1);
        let original = s.selection.ids().next();
        let before = s.history_len();
        let made = s.execute("array.create", &json!({
            "mode": "linear", "count": 10, "dx": 20, "dy": 0,
            "alpha_stops": [
                {"offset": 0.0, "alpha": 0.75},
                {"offset": 0.4, "alpha": 1.0},
                {"offset": 1.0, "alpha": 0.03}
            ]
        }));
        assert!(made.is_ok(), "{made:?}");
        assert_eq!(s.history_len(), before + 1, "baking an array is a single undo step");
        assert_eq!(s.selection.len(), 10, "count includes the original");
        assert_eq!(s.selection.ids().next(), original, "instance #1 keeps the source ID");
        let elements = s.selected_elements();
        assert_eq!(elements.len(), 10);
        let alphas: Vec<f32> = elements.iter().map(|(_, e)| e.color.alpha()).collect();
        assert!((alphas[0] - 0.75).abs() < 1e-4);
        assert!((alphas[9] - 0.03).abs() < 1e-4);
        assert!(s.execute("edit.undo", &json!({})).is_ok());
        assert_eq!(s.selected_elements().len(), 1, "undo restores the original single instance");
    }

    #[test]
    fn stop_positions_and_values_are_validated() {
        let p = json!({"alpha_stops": [{"offset": 1, "alpha": 0.03}, {"offset": 0, "alpha": 0.75}, {"offset": 0.4, "alpha": 1}]});
        let parsed = parse_stops(&p).ok().flatten().unwrap_or_default();
        assert_eq!(parsed.len(), 3);
        assert_eq!(parsed[0].alpha, 0.75);
        assert_eq!(parsed[1].offset, 0.4);
        assert!(parse_stops(&json!({"alpha_stops": [{"offset": 2, "alpha": 0.5}, {"offset": 1, "alpha": 0.4}]})).is_err());
        for (key, value) in [("alpha", json!(1.01)), ("scale_x", json!(101)), ("scale_y", json!("50%")), ("rotation", json!(1e10))] {
            let mut params = json!({"property_stops": [{"offset": 0}, {"offset": 1}]});
            params["property_stops"][0][key] = value;
            assert!(parse_stops(&params).is_err());
        }
    }

    #[test]
    fn preview_owns_its_gesture_and_recovery_sees_the_committed_document() {
        let mut s = Session::default();
        s.execute("draw.rect", &json!({"x": 10, "y": 10, "w": 24, "h": 14})).unwrap();
        s.execute("modify.convert_to_symbol", &json!({"name": "Source"})).unwrap();
        s.mark_saved();
        let (doc, selection, state, steps) = (s.doc.clone(), s.selection.clone(), s.state, s.history_len());

        s.begin_gesture("Other edit");
        assert!(s.preview_array(&json!({"count": 3})).is_err());
        assert!(s.in_gesture() && !s.array_preview_active());
        s.end_gesture();
        s.preview_array(&json!({"count": 3})).unwrap();
        let shown = s.doc.clone();
        s.end_gesture();
        s.revert_gesture();
        assert!(s.undo().is_err());
        assert!(s.redo().is_err());
        assert!(s.execute("edit.deselect", &json!({})).is_err());
        assert!(s.execute("file.save", &json!({})).is_err());
        assert!(s.doc == shown && s.array_preview_active() && s.in_gesture());
        assert_eq!(s.selection.len(), 3);
        assert_eq!(s.history_len(), steps);
        assert!(!s.is_dirty());
        assert!(s.committed_document() == (&doc, state));

        assert!(s.preview_array(&json!({"count": 0})).is_err());
        assert!(s.array_preview_active() && s.doc == doc && s.selection == selection);
        s.preview_array(&json!({"count": 2})).unwrap();
        s.finish_array_preview(false);
        assert!(s.doc == doc && s.selection == selection && !s.in_gesture());
        assert_eq!(s.history_len(), steps);
    }

    #[test]
    fn array_requires_the_entire_selection_to_be_one_instance_on_the_active_layer() {
        let mut s = Session::default();
        s.execute("draw.rect", &json!({"x": 10, "y": 10, "w": 24, "h": 14})).unwrap();
        s.execute("modify.convert_to_symbol", &json!({"name": "Source"})).unwrap();
        let (layer, source) = s.selection.first().unwrap();
        s.execute("layer.new", &json!({"name": "Others"})).unwrap();
        s.execute("draw.rect", &json!({"x": 50, "y": 20, "w": 10, "h": 10})).unwrap();
        let (other_layer, other) = s.selection.first().unwrap();
        s.selection.insert(layer, source);
        s.layer = layer;
        assert!(s.execute("array.create", &json!({})).is_err());
        assert!(s.selection.contains(other_layer, other));
        s.select_only([source]);
        assert!(s.execute("array.create", &json!({"count": 1})).is_ok());
        assert_eq!(s.selection.first(), Some((layer, source)));
    }

    #[test]
    fn moving_linear_preview_reuses_instances_and_commits_the_same_document_as_the_command() {
        let mut s = Session::default();
        s.execute("draw.rect", &json!({"x": 10, "y": 10, "w": 24, "h": 14})).unwrap();
        s.execute("modify.convert_to_symbol", &json!({"name": "Source"})).unwrap();
        let (base, selected, steps) = (s.doc.clone(), s.selection.clone(), s.history_len());
        let mut p = json!({"mode": "linear", "count": 5, "start_x": 50, "start_y": 40, "dx": 12, "dy": -5,
            "stacking": "front_to_back", "property_stops": [{"offset": 0, "alpha": 1, "scale_x": 0.6},
            {"offset": 1, "alpha": 0.2, "scale_x": 1.8, "scale_y": 1.5, "rotation": -540}]});
        s.preview_array(&p).unwrap();
        let generation = s.array_translation_generation().unwrap();
        let ids = s.selection.clone();
        let still = s.selected_elements().last().unwrap().1.clone();
        p["dx"] = json!(400);
        p["dy"] = json!(70);
        s.preview_array(&p).unwrap();
        assert_eq!(s.array_translation_generation(), Some(generation));
        assert_eq!(s.selection, ids);
        assert!(Arc::ptr_eq(&still, &s.selected_elements().last().unwrap().1));

        let mut expected = Session::new(base.clone());
        expected.selection = selected.clone();
        expected.execute("array.create", &p).unwrap();
        assert!(s.doc == expected.doc && s.selection == expected.selection);
        s.finish_array_preview(true);
        assert!(s.array_translation_generation().is_none());
        assert_eq!(s.history_len(), steps + 1);
        let (_, recorded) = s.history.steps().last().unwrap();
        assert_eq!(recorded.unwrap(), &[crate::history::Recorded::new("array.create", &p)]);
        s.undo().unwrap();
        assert!(s.doc == base && s.selection == selected);
    }
}
