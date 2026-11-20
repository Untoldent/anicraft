//! Baked symbol arrays and the editor's reversible preview of the same command.
//!
//! A count of N produces N total instances, including the original (ID preserved).
//! The first instance moves to the requested start location. Copies are ordinary
//! symbol instances, not a new document object, so existing editors and exports work.
//! Geometry edits that only move instances reuse the existing preview.

use crate::params::{opt_bool, opt_f32, opt_str, opt_u32};
use crate::{CommandSpec, EngineError, Result, Session};
use anicraft_doc::{ColorEffect, ColorStyle, Easing, Element, ElementId, ElementKind};
use anicraft_geom::{Affine, Vec2};
use serde_json::{Value, json};
use std::ops::Range;
use std::sync::Arc;

const MAX_COPIES: u32 = 500;
pub const CLOSED_SWEEP_LIMIT: f32 = 360.0;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ArrayEaseMode {
    #[default]
    Global,
    PerSegment,
}

impl ArrayEaseMode {
    pub const fn key(self) -> &'static str {
        match self {
            Self::Global => "global",
            Self::PerSegment => "per_segment",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct ArrayEasing {
    pub mode: ArrayEaseMode,
    pub preset: Easing,
    pub distribution: bool,
    pub transform: bool,
    pub colour_effect: bool,
}

impl Default for ArrayEasing {
    fn default() -> Self {
        Self {
            mode: ArrayEaseMode::Global, preset: Easing::Linear,
            distribution: true, transform: true, colour_effect: true,
        }
    }
}

impl ArrayEasing {
    fn parse(params: &Value) -> Result<Self> {
        let defaults = Self::default();
        let Some(easing) = params.get("easing") else { return Ok(defaults) };
        if !easing.is_object() {
            return Err(EngineError::Param("easing must be an object".into()));
        }
        let mode = match opt_str(easing, "mode")? {
            None | Some("global") => ArrayEaseMode::Global,
            Some("per_segment") => ArrayEaseMode::PerSegment,
            Some(other) => return Err(EngineError::Param(format!("unknown Array easing mode '{other}'"))),
        };
        let preset = opt_named_ease(easing, "preset")?.unwrap_or(defaults.preset);
        Ok(Self {
            mode, preset,
            distribution: opt_bool(easing, "distribution")?.unwrap_or(defaults.distribution),
            transform: opt_bool(easing, "transform")?.unwrap_or(defaults.transform),
            colour_effect: opt_bool(easing, "colour_effect")?.unwrap_or(defaults.colour_effect),
        })
    }

    // A single Per Segment preset remains valid for command recordings without stop curves.
    fn segment_default(&self, stops: &[ArrayPropertyStop]) -> &Easing {
        if stops.iter().any(|stop| stop.outbound.is_some()) { &Easing::Linear } else { &self.preset }
    }

    fn eases_distribution(&self, stops: &[ArrayPropertyStop]) -> bool {
        self.distribution && match self.mode {
            ArrayEaseMode::Global => self.preset != Easing::Linear,
            ArrayEaseMode::PerSegment => *self.segment_default(stops) != Easing::Linear
                || stops.iter().take(stops.len().saturating_sub(1))
                    .any(|stop| stop.outbound.as_ref().is_some_and(|ease| *ease != Easing::Linear)),
        }
    }

    fn eased_position(&self, stops: &[ArrayPropertyStop], t: f64) -> f64 {
        let progress = t as f32;
        if self.mode == ArrayEaseMode::Global {
            return f64::from(self.preset.apply(progress));
        }
        // The path always has boundaries at 0 and 1, even when the authored stops don't.
        let next = stops.partition_point(|stop| stop.offset <= progress);
        let left = next.checked_sub(1).and_then(|i| stops.get(i));
        let right = stops.get(next);
        let a = left.map_or(0.0, |stop| stop.offset);
        let b = right.map_or(1.0, |stop| stop.offset);
        let span = b - a;
        if span <= f32::EPSILON || progress == a {
            return t;
        }
        let outbound = left.filter(|_| right.is_some()).and_then(|stop| stop.outbound.as_ref())
            .unwrap_or_else(|| self.segment_default(stops));
        if *outbound == Easing::Linear { return t; }
        f64::from(a + span * outbound.apply((progress - a) / span))
    }

    fn properties(&self, stops: &[ArrayPropertyStop], t: f32) -> ArrayPropertyStop {
        let linear = || ArrayPropertyStop::sample(stops.iter(), t);
        if !(self.transform || self.colour_effect) || (self.mode == ArrayEaseMode::Global && self.preset == Easing::Linear) {
            return linear();
        }
        let mut eased = match self.mode {
            ArrayEaseMode::Global => ArrayPropertyStop::sample_global(stops, self.preset.apply(t)),
            ArrayEaseMode::PerSegment => ArrayPropertyStop::sample_segments(stops.iter(), t, Some(self.segment_default(stops))),
        };
        if !self.transform {
            let base = linear();
            eased.scale = base.scale;
            eased.rotation = base.rotation;
        } else if !self.colour_effect {
            let base = linear();
            eased.alpha = base.alpha;
            eased.color = base.color;
        }
        eased
    }
}

fn opt_named_ease(p: &Value, key: &str) -> Result<Option<Easing>> {
    opt_str(p, key)?.map(|name| {
        Easing::from_name(name).filter(|ease| ease.name().is_some())
            .ok_or_else(|| EngineError::Param(format!("unknown Array easing preset '{name}'")))
    }).transpose()
}

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
#[derive(Clone, Debug, PartialEq)]
pub struct ArrayPropertyStop {
    pub offset: f32,
    pub alpha: f32,
    /// The stop's colour style. The Advanced alpha multiplier lives in `alpha`.
    pub color: ColorEffect,
    pub scale: [f32; 2],
    pub rotation: f32,
    /// None denotes a stop from a command with no authored outbound preset.
    pub outbound: Option<Easing>,
}

impl ArrayPropertyStop {
    pub const LIMIT: usize = 15;
    pub const SCALE_LIMIT: f32 = 100.0;
    pub const ROTATION_LIMIT: f32 = 100_000.0;

    pub const fn new(offset: f32, alpha: f32) -> Self {
        Self { offset, alpha, color: ColorEffect::None, scale: [1.0; 2], rotation: 0.0, outbound: None }
    }

    /// Hold the endpoint value outside the first or last stop, as gradients do.
    /// Both the editor and command pass in stops sorted by position.
    pub fn sample<'a>(stops: impl IntoIterator<Item = &'a Self>, t: f32) -> Self {
        Self::sample_segments(stops, t, None)
    }

    fn sample_segments<'a>(stops: impl IntoIterator<Item = &'a Self>, t: f32, segment_default: Option<&Easing>) -> Self {
        let mut stops = stops.into_iter();
        let Some(mut a) = stops.next() else { return Self::new(t, 1.0) };
        if t <= a.offset { return Self { offset: t, ..a.clone() }; }
        for b in stops {
            if t <= b.offset {
                let span = b.offset - a.offset;
                if span <= f32::EPSILON || t == b.offset { return Self { offset: t, ..b.clone() }; }
                let ease = segment_default.map_or(&Easing::Linear, |default| a.outbound.as_ref().unwrap_or(default));
                return a.blend(b, t, ease.apply((t - a.offset) / span));
            }
            a = b;
        }
        Self { offset: t, ..a.clone() }
    }

    fn sample_global(stops: &[Self], t: f32) -> Self {
        if (0.0..=1.0).contains(&t) {
            return Self::sample(stops.iter(), t);
        }
        // Continue from the value at the edge of the track using the nearest usable
        // stop span. A held margin inside the track keeps its original value.
        let (boundary, end, other) = if t < 0.0 {
            let Some(end) = stops.first() else { return Self::new(t, 1.0) };
            (0.0, end, stops.iter().find(|stop| stop.offset - end.offset > f32::EPSILON))
        } else {
            // At a coincident endpoint, match the value that sampling exactly 1 returns.
            let end = stops.get(stops.partition_point(|stop| stop.offset < 1.0)).or_else(|| stops.last());
            let Some(end) = end else { return Self::new(t, 1.0) };
            (1.0, end, stops.iter().rev().find(|stop| end.offset - stop.offset > f32::EPSILON))
        };
        other.map_or_else(|| Self { offset: t, ..end.clone() }, |other| {
            end.blend(other, t, (t - boundary) / (other.offset - end.offset))
        })
    }

    fn blend(&self, other: &Self, t: f32, f: f32) -> Self {
        let blend = |a, b| a + (b - a) * f;
        Self {
            offset: t,
            alpha: blend(self.alpha, other.alpha).clamp(alpha_min(self.color).min(alpha_min(other.color)), 1.0),
            color: self.color.lerp(&other.color, f).sanitized(),
            scale: [
                blend(self.scale[0], other.scale[0]).clamp(-Self::SCALE_LIMIT, Self::SCALE_LIMIT),
                blend(self.scale[1], other.scale[1]).clamp(-Self::SCALE_LIMIT, Self::SCALE_LIMIT),
            ],
            rotation: blend(self.rotation, other.rotation),
            outbound: None,
        }
    }

    /// Apply the Array colour once, after the effect already authored on the source symbol.
    fn apply_color(&self, source: ColorEffect) -> ColorEffect {
        let effect = match self.color {
            ColorEffect::None => ColorEffect::opacity(self.alpha),
            color if self.alpha == 1.0 => color,
            color => color.with_alpha(self.alpha),
        };
        if effect.transform().is_identity() && source != ColorEffect::None {
            source
        } else {
            effect.nested(&source)
        }
    }

    fn apply_transform(&self, base: Affine, pivot: Option<Vec2>) -> Affine {
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

fn alpha_min(color: ColorEffect) -> f32 {
    if matches!(color, ColorEffect::Advanced(_)) { *anicraft_doc::ADVANCED_MUL.start() } else { 0.0 }
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
    eased_progress: Option<Vec<f64>>,
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
        let (layout, _) = explicit_positions(p)?;
        let easing = ArrayEasing::parse(p).ok()?;
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
        let custom_pivot = copies.first()?.pivot().is_some() && p.get("property_stops").is_some();
        let stops = parse_stops(p).ok()?.unwrap_or_default();
        // A transformed custom pivot can offset the copy from its layout position.
        // Replay from the source so those offsets stay exact after a move.
        if custom_pivot && stops.iter().any(|stop| stop.scale != [1.0; 2] || stop.rotation != 0.0) {
            return None;
        }
        let eased_progress = easing.eases_distribution(&stops).then(|| {
            let count = count as u32;
            (0..count).map(|i| easing.eased_position(&stops, layout.progress(i, count))).collect()
        });
        let ids = copies.iter().map(|e| e.id);
        let in_order = if reversed { ids.rev().eq(s.selection.ids()) } else { ids.eq(s.selection.ids()) };
        in_order.then(|| Self { generation: crate::history::fresh_state(), key, instances, reversed, eased_progress, params: p.clone() })
    }

    fn move_to(&mut self, s: &mut Session, params: &Value) -> bool {
        let (Some(old), Some(new), Some((layout, coords))) = (self.params.as_object(), params.as_object(), explicit_positions(params)) else { return false };
        if old.len() != new.len() || old.iter().any(|(key, v)| !coords.contains(&key.as_str()) && new.get(key) != Some(v)) {
            return false;
        }
        if old == new { return true; }
        let count = self.instances.len() as u32;
        let place = |i, fraction| layout.place(Affine::IDENTITY, i, count, fraction).translation();
        // Check every eased sample before changing anything. Back Out can exceed both the
        // requested span and the nominal last sample, so checking endpoints isn't enough.
        let eased_positions = if let Some(progress) = &self.eased_progress {
            let positions: Vec<_> = progress.iter().enumerate().map(|(i, &t)| place(i as u32, Some(t))).collect();
            if !positions.iter().all(|at| at.x.is_finite() && at.y.is_finite()) { return false; }
            Some(positions)
        } else {
            if ![0, count.saturating_sub(1)].into_iter().all(|i| {
                let at = place(i, None);
                at.x.is_finite() && at.y.is_finite()
            }) { return false; }
            None
        };
        let Some(instances) = s.layer_mut(None).ok().and_then(|layer| layer.keyframe_mut(self.key))
            .and_then(|kf| kf.elements.get_mut(self.instances.clone())) else { return false };
        let mut changed = false;
        for (i, el) in instances.iter_mut().enumerate() {
            let i = if self.reversed { count - 1 - i as u32 } else { i as u32 };
            let at = eased_positions.as_ref().and_then(|positions| positions.get(i as usize)).copied()
                .unwrap_or_else(|| place(i, None));
            if at != el.transform.translation() {
                let xf = &mut Arc::make_mut(el).transform;
                xf.m[4] = at.x;
                xf.m[5] = at.y;
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
        params: "{mode: linear|radial, count: 1..500 (includes original), dx?, dy?, start_x?, start_y? (linear), center_x?, center_y?, radius?, start_angle?, sweep_angle?, closed_loop?, spiral_offset?, drift_x?, drift_y?, rotate? (radial), stacking?: back_to_front|front_to_back, property_stops?: [{offset: 0..1, alpha?: 0..1 (-1..1 for Advanced), color_effect?: document Colour Effect object, scale_x?: -100..100, scale_y?: -100..100, rotation?: -100000..100000, outbound_ease?: named ease}, ...] (2..15), alpha_stops?: [{offset: 0..1, alpha: 0..1}, ...] (2..15, legacy alternative), easing?: {mode?: global|per_segment, preset?: named ease (e.g. Linear, QuadIn, BackOut, ElasticInOut), distribution?, transform?, colour_effect?}}. Easing defaults to Global Linear with all three families enabled. In Per Segment mode each stop eases toward the next using outbound_ease (default Linear). If no stop supplies outbound_ease, preset supplies the legacy shared segment curve. Scale is a local multiplier (default 1), rotation is an additive angle in degrees (default 0), and Alpha is a multiplier (default 1). When color_effect is supplied, the stop effects are composed over the source and alpha overrides the effect's alpha multiplier. Older calls without color_effect retain absolute alpha. Radial sweep defaults to 360 degrees. Closed loop defaults to true, uses count intervals and limits sweep to one turn. Open mode uses count minus one intervals. spiral_offset changes radius down to zero and drift_x/drift_y move the centre. All three rates default to zero and are measured in pixels per turn. Requires exactly one selected symbol instance on the active layer. Moves the original to position 1, bakes the copies and selects them all.",
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
        let color = match v.get("color_effect").filter(|_| !legacy) {
            None => ColorEffect::None,
            Some(raw) => {
                if !raw.is_object() || opt_str(raw, "style")?.is_some_and(|s| ColorStyle::from_key(s).is_none()) {
                    return Err(EngineError::Param("color_effect must be a supported Colour Effect object".into()));
                }
                serde_json::from_value::<ColorEffect>(raw.clone())
                    .map_err(|e| EngineError::Param(format!("invalid color_effect: {e}")))?
            }
        };
        let alpha = number("alpha", alpha_min(color), 1.0, (!legacy).then(|| color.alpha()))?;
        let mut stop = ArrayPropertyStop::new(offset, alpha);
        if !legacy {
            stop.color = match color {
                ColorEffect::Alpha(_) => ColorEffect::None,
                ColorEffect::Advanced(_) => color.with_alpha(1.0),
                color => color,
            };
            let scale = ArrayPropertyStop::SCALE_LIMIT;
            let rotation = ArrayPropertyStop::ROTATION_LIMIT;
            stop.scale = [number("scale_x", -scale, scale, Some(1.0))?, number("scale_y", -scale, scale, Some(1.0))?];
            stop.rotation = number("rotation", -rotation, rotation, Some(0.0))?;
            stop.outbound = opt_named_ease(v, "outbound_ease")?;
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
    fn progress(self, index: u32, count: u32) -> f64 {
        let closed_loop = matches!(self, Layout::Radial(radial) if radial.closed_loop);
        let divisor = count.saturating_sub(u32::from(!closed_loop)).max(1);
        f64::from(index) / f64::from(divisor)
    }

    fn place(self, source: Affine, index: u32, count: u32, eased: Option<f64>) -> Affine {
        match self {
            Layout::Linear { start_x, start_y, dx, dy } => {
                let mut m = source;
                let step = eased.map_or(index as f32, |t| (t * f64::from(count.saturating_sub(1))) as f32);
                m.m[4] = start_x + step * dx;
                m.m[5] = start_y + step * dy;
                m
            }
            Layout::Radial(radial) => {
                let t = eased.unwrap_or_else(|| self.progress(index, count));
                // Rotate the basis by the signed travel, then set the registration point.
                let mut m = if radial.rotate { Affine::rotate(radial.travel(t).to_radians() as f32).mul(&source) } else { source };
                let at = radial.point(t);
                m.m[4] = at.x;
                m.m[5] = at.y;
                m
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
    let easing = ArrayEasing::parse(p)?;
    let eased_distribution = easing.eases_distribution(stops.as_deref().unwrap_or_default());
    // Old Array command recordings used an absolute alpha multiplier. The full colour
    // controls use a relative effect, even when every stop is set to the neutral style.
    let compose_colour = p.get("property_stops").and_then(Value::as_array)
        .is_some_and(|stops| stops.iter().any(|stop| stop.get("color_effect").is_some()));
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
        let t = layout.progress(i, count);
        let eased = eased_distribution.then(|| easing.eased_position(stops.as_deref().unwrap_or_default(), t));
        let base = layout.place(original.transform, i, count, eased);
        let stop = stops.as_deref().map(|stops| easing.properties(stops, t as f32));
        let transform = stop.as_ref().map_or(base, |stop| stop.apply_transform(base, original.pivot()));
        if !transform.is_finite() {
            return Err(EngineError::Param("array positions exceeded finite coordinate range".into()));
        }
        let mut el = original.clone();
        // Instance #1 is the original and keeps its ID.
        if i > 0 { el.id = ElementId(s.doc.alloc_id()); }
        el.transform = transform;
        if let Some(stop) = stop {
            el.color = if compose_colour { stop.apply_color(original.color) } else { original.color.with_alpha(stop.alpha) };
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
    use anicraft_doc::{ColorTransform, Rgba};

    fn st(p: f32, alpha: f32) -> ArrayPropertyStop { ArrayPropertyStop::new(p, alpha) }

    #[test]
    fn independent_stops_can_fade_out_then_back_in() {
        let stops = [st(0.0, 1.0), st(0.5, 0.0), st(1.0, 1.0)];
        assert!((ArrayPropertyStop::sample(&stops, 0.25).alpha - 0.5).abs() < 1e-6);
        assert!((ArrayPropertyStop::sample(&stops, 0.75).alpha - 0.5).abs() < 1e-6);
        assert_eq!(ArrayPropertyStop::sample(&stops, 0.5).alpha, 0.0);
    }

    #[test]
    fn colour_stops_interpolate_signed_brightness_tint_and_mixed_styles() {
        let dark = ArrayPropertyStop { color: ColorEffect::Brightness(-0.5), ..st(0.0, 0.5) };
        let light = ArrayPropertyStop { color: ColorEffect::Brightness(0.5), ..st(1.0, 1.0) };
        let halfway = ArrayPropertyStop::sample([&dark, &light], 0.5);
        assert_eq!(halfway.color, ColorEffect::Brightness(0.0));
        assert_eq!(halfway.alpha, 0.75);

        let red = ArrayPropertyStop { color: ColorEffect::Tint { color: Rgba::hex(0xff0000), amount: 0.25 }, ..st(0.0, 0.25) };
        let blue = ArrayPropertyStop { color: ColorEffect::Tint { color: Rgba::hex(0x0000ff), amount: 0.75 }, ..st(1.0, 0.75) };
        assert_eq!(ArrayPropertyStop::sample([&red, &blue], 0.5).color,
            ColorEffect::Tint { color: Rgba::hex(0xff0000).lerp(Rgba::hex(0x0000ff), 0.5), amount: 0.5 });
        assert_eq!(ArrayPropertyStop::sample([&st(0.0, 1.0), &blue], 0.5).color,
            ColorEffect::Tint { color: Rgba::hex(0x0000ff), amount: 0.375 });
        let mixed = ArrayPropertyStop::sample([&red, &light], 0.5);
        assert_eq!(mixed.color, ColorEffect::Advanced(ColorTransform {
            mul: [0.625, 0.625, 0.625, 1.0], add: [0.375, 0.25, 0.25, 0.0],
        }));
        assert_eq!(mixed.apply_color(ColorEffect::None).alpha(), 0.625);
    }

    #[test]
    fn colour_easing_samples_all_advanced_channels_and_bounds_overshoot() {
        let end = ArrayPropertyStop {
            color: ColorEffect::Advanced(ColorTransform {
                mul: [-1.0, 0.5, 0.0, 1.0], add: [1.0, -1.0, 0.5, 0.25],
            }),
            ..st(0.5, -1.0)
        };
        let start = ArrayPropertyStop { outbound: Some(Easing::QuadIn), ..st(0.0, 1.0) };
        let stops = [start, end.clone(), ArrayPropertyStop { offset: 1.0, ..end.clone() }];
        let global = ArrayEasing { preset: Easing::QuadIn, ..ArrayEasing::default() };
        let segment = ArrayEasing { mode: ArrayEaseMode::PerSegment, ..global.clone() };
        for (easing, fraction) in [(&global, 0.125), (&segment, 0.25)] {
            let actual = easing.properties(&stops, 0.25).apply_color(ColorEffect::None).transform();
            assert_eq!(actual, ColorTransform {
                mul: [1.0 - 2.0 * fraction, 1.0 - 0.5 * fraction, 1.0 - fraction, 1.0 - 2.0 * fraction],
                add: [fraction, -fraction, 0.5 * fraction, 0.25 * fraction],
            });
            let disabled = ArrayEasing { colour_effect: false, ..easing.clone() };
            let actual = disabled.properties(&stops, 0.25).apply_color(ColorEffect::None).transform();
            assert_eq!(actual.mul, [0.0, 0.75, 0.5, 0.0]);
            assert_eq!(actual.add, [0.5, -0.5, 0.25, 0.125]);
        }

        let back = ArrayEasing { preset: Easing::BackOut, ..global };
        let mut stops = [stops[0].clone(), ArrayPropertyStop { offset: 1.0, ..end }];
        stops[0].outbound = Some(Easing::BackOut);
        for easing in [&back, &ArrayEasing { mode: ArrayEaseMode::PerSegment, ..back.clone() }] {
            let t = easing.properties(&stops, 0.625).apply_color(ColorEffect::None).transform();
            assert_eq!(t.mul[0], -1.0);
            assert_eq!(t.mul[3], -1.0);
            assert_eq!(&t.add[..2], &[1.0, -1.0]);
            assert!(t.add[3] > 0.25);
        }
        let tint = |offset, color, amount| ArrayPropertyStop {
            color: ColorEffect::Tint { color: Rgba::hex(color), amount }, ..st(offset, 1.0)
        };
        let stops = [tint(0.0, 0xff0000, 0.0), tint(1.0, 0x0040ff, 1.0)];
        let ColorEffect::Tint { color, amount } = back.properties(&stops, 0.625).color else { panic!("expected Tint"); };
        assert_eq!(amount, 1.0);
        assert_eq!(color.r, 0.0);
        assert_eq!(color.b, 1.0);
        assert!(color.g > Rgba::hex(0x0040ff).g);
    }

    #[test]
    fn colour_preview_and_commit_compose_with_source_and_render_in_both_layouts() {
        let mut original = Session::default();
        original.execute("draw.rect", &json!({"x": 15, "y": 15, "w": 10, "h": 10, "fill": "#336699", "stroke": null})).unwrap();
        original.execute("modify.convert_to_symbol", &json!({"name": "Source"})).unwrap();
        let source = ColorEffect::Advanced(ColorTransform { mul: [1.0, 0.5, 1.0, 0.5], add: [0.0; 4] });
        original.execute("element.set_color_effect", &json!({"green_multiplier": 50, "alpha_multiplier": 50})).unwrap();
        let base = original.doc.clone();
        let selection = original.selection.clone();
        let bake = |s: &Session| anicraft_render::render_frame(&s.doc, 0, 1.0).unwrap();
        let pixel = |s: &Session, at: [f32; 2]| {
            let pm = bake(s);
            let i = ((at[1] as u32 * pm.width + at[0] as u32) * 4) as usize;
            <[u8; 4]>::try_from(&pm.to_rgba8()[i..i + 4]).unwrap()
        };
        let last = ColorEffect::Advanced(ColorTransform { mul: [0.0, 0.0, 0.0, 0.25], add: [0.0, 0.0, 1.0, 0.375] });
        for (mode, positions) in [("linear", [[20.0, 20.0], [60.0, 20.0], [100.0, 20.0]]),
            ("radial", [[180.0, 100.0], [105.0, 105.0], [230.0, 110.0]])]
        {
            let params = json!({
                "mode": mode, "count": 3, "start_x": 20, "start_y": 20, "dx": 40,
                "center_x": 140, "center_y": 100, "radius": 40, "start_angle": 0,
                "sweep_angle": -360, "closed_loop": false, "spiral_offset": 20, "drift_x": 30, "drift_y": 10,
                "property_stops": [
                    {"offset": 0, "color_effect": ColorEffect::None},
                    {"offset": 0.5, "alpha": 0.5, "color_effect": ColorEffect::Brightness(0.5)},
                    {"offset": 1, "alpha": 0.5, "color_effect": last},
                ],
            });
            let mut s = Session::new(base.clone());
            s.selection = selection.clone();
            let mut draft = params.clone();
            draft["property_stops"][2].as_object_mut().unwrap().remove("alpha");
            s.preview_array(&draft).unwrap();
            assert_eq!(s.selected_elements()[2].1.color, last.nested(&source));
            s.finish_array_preview(false);
            assert_eq!(s.doc, base);
            assert_eq!(s.selection, selection);
            assert_eq!(s.history_len(), 0);

            s.preview_array(&params).unwrap();
            let elements = s.selected_elements();
            assert_eq!(elements[0].1.color, source);
            assert_eq!(elements[1].1.color, ColorEffect::Brightness(0.5).with_alpha(0.5).nested(&source));
            assert_eq!(elements[2].1.color, last.with_alpha(0.5).nested(&source));
            for ((_, el), [x, y]) in elements.iter().zip(positions) {
                assert_eq!(el.transform.translation(), Vec2::new(x, y));
            }
            // Alpha = 0.5 * 0.5 + 0.375. Blue over the default white stage is (96, 96, 255).
            let colour = pixel(&s, positions[2]);
            assert!(colour.iter().zip([96u8, 96, 255, 255]).all(|(a, b)| a.abs_diff(b) <= 1), "{colour:?}");
            let preview = bake(&s).to_rgba8();
            s.finish_array_preview(true);
            assert_eq!(s.history_len(), 1);
            assert_eq!(bake(&s).to_rgba8(), preview);

            let (_, recorded) = s.history.steps().last().unwrap();
            let [recorded] = recorded.unwrap() else { panic!("expected one recorded Array command") };
            assert_eq!(recorded.cmd, "array.create");
            let replay_params = serde_json::from_str(&recorded.params.to_string()).unwrap();
            let mut replay = Session::new(base.clone());
            replay.selection = selection.clone();
            replay.execute(&recorded.cmd, &replay_params).unwrap();
            assert_eq!(replay.doc, s.doc);

            s.undo().unwrap();
            assert_eq!(s.doc, base);
        }
        original.execute("array.create", &json!({"count": 1, "property_stops": [
            {"offset": 0, "alpha": 1}, {"offset": 1, "alpha": 1}
        ]})).unwrap();
        assert_eq!(original.selected_elements()[0].1.color, source.with_alpha(1.0));
    }

    #[test]
    fn array_easing_accepts_the_shared_named_catalogue_and_command_names() {
        for preset in Easing::named() {
            let name = crate::params::easing_json(&preset);
            let easing = ArrayEasing::parse(&json!({"easing": {"preset": name}})).unwrap();
            assert_eq!(easing.preset, preset);
            for t in [0.0, 0.25, 0.75, 1.0] {
                assert_eq!(easing.eased_position(&[], t), f64::from(preset.apply(t as f32)));
            }
        }
        let alias = ArrayEasing::parse(&json!({"easing": {"preset": "elastic-in-out"}})).unwrap();
        assert_eq!(alias.preset, Easing::ElasticInOut);
    }

    #[test]
    fn array_easing_modes_and_families_sample_the_authored_stops_independently() {
        let stops = [
            st(0.0, 0.2),
            ArrayPropertyStop { scale: [2.0, 0.5], rotation: 360.0, ..st(0.5, 0.6) },
            ArrayPropertyStop { scale: [3.0, 0.25], rotation: 720.0, ..st(1.0, 0.4) },
        ];
        let easing = ArrayEasing { preset: Easing::QuadIn, ..ArrayEasing::default() };
        let per_segment = ArrayEasing { mode: ArrayEaseMode::PerSegment, ..easing.clone() };
        for (easing, at, angle) in [(&easing, 0.0625, 45.0), (&per_segment, 0.125, 90.0)] {
            assert_eq!(easing.eased_position(&stops, 0.25), at);
            let eased = easing.properties(&stops, 0.25);
            assert_eq!(eased.rotation, angle);
            assert_eq!(eased.scale, [1.0 + at as f32 * 2.0, 1.0 - at as f32]);
            let linear = ArrayPropertyStop::sample(&stops, 0.25);
            let colour_only = ArrayEasing { transform: false, ..easing.clone() }.properties(&stops, 0.25);
            assert_eq!(colour_only.scale, linear.scale);
            assert_eq!(colour_only.rotation, linear.rotation);
            assert_eq!(colour_only.alpha, eased.alpha);
            let transform_only = ArrayEasing { colour_effect: false, ..easing.clone() }.properties(&stops, 0.25);
            assert_eq!(transform_only.scale, eased.scale);
            assert_eq!(transform_only.alpha, linear.alpha);
        }
        assert_eq!(per_segment.properties(&stops, 0.5), stops[1]);
        assert_eq!(per_segment.eased_position(&stops, 0.5), 0.5);
        assert_eq!(per_segment.properties(&stops, 1.0), stops[2]);
        assert_ne!(easing.properties(&stops, 0.5), stops[1]);
        let interior = [st(0.25, 0.2), st(0.75, 0.8)];
        assert_eq!(per_segment.eased_position(&interior, 0.125), 0.0625);
        assert_eq!(per_segment.eased_position(&interior, 0.875), 0.8125);
        assert_eq!(per_segment.properties(&interior, 0.125).alpha, 0.2);
        assert_eq!(per_segment.properties(&interior, 0.875).alpha, 0.8);
    }

    #[test]
    fn array_back_overshoots_using_the_current_segment_and_handles_coincident_stops() {
        let stops = [
            st(0.0, 0.0),
            ArrayPropertyStop { scale: [1.5, 0.5], rotation: 360.0, ..st(0.5, 1.0) },
            ArrayPropertyStop { scale: [2.0, 0.25], rotation: 720.0, ..st(1.0, 0.0) },
        ];
        let global = ArrayEasing { preset: Easing::BackOut, ..ArrayEasing::default() };
        let per_segment = ArrayEasing { mode: ArrayEaseMode::PerSegment, ..global.clone() };
        assert!(global.eased_position(&stops, 0.625) > 1.0);
        let overshoot = global.properties(&stops, 0.625);
        assert!(overshoot.rotation > 720.0 && overshoot.scale[0] > 2.0 && overshoot.scale[1] < 0.25);
        assert_eq!(overshoot.alpha, 0.0);
        let local = per_segment.properties(&stops, 0.3125);
        assert!(per_segment.eased_position(&stops, 0.3125) > 0.5);
        assert!(local.rotation > 360.0 && local.scale[0] > 1.5 && local.scale[1] < 0.5);
        assert_eq!(local.alpha, 1.0);

        let end = ArrayPropertyStop { rotation: -90.0, ..st(1.0, 0.25) };
        let near = ArrayPropertyStop { offset: 1.0 - f32::EPSILON / 2.0, ..end.clone() };
        let crowded = [stops[0].clone(), stops[1].clone(), near, stops[2].clone(), end.clone()];
        assert_eq!(ArrayPropertyStop::sample_global(&crowded, 1.0).rotation, 720.0);
        assert!(ArrayPropertyStop::sample_global(&crowded, 1.001).rotation > 720.0);
        let coincident = [end, stops[2].clone()];
        assert_eq!(per_segment.properties(&coincident, 0.95).rotation, -90.0);
        assert_eq!(global.properties(&coincident, 0.625).rotation, -90.0);

        let back_in = ArrayEasing { preset: Easing::BackIn, ..global };
        assert!(back_in.eased_position(&stops, 0.375) < 0.0);
        let before_start = back_in.properties(&stops, 0.375);
        assert!(before_start.rotation < 0.0 && before_start.scale[0] < 1.0);
        assert_eq!(before_start.alpha, 0.0);
        let local = ArrayEasing { mode: ArrayEaseMode::PerSegment, ..back_in };
        assert!(local.eased_position(&stops, 0.6875) < 0.5);
        assert!(local.properties(&stops, 0.6875).rotation < 360.0);
    }

    #[test]
    fn authored_outbound_presets_are_independent_and_follow_stop_order() {
        let params = json!({
            "easing": {"mode": "per_segment", "preset": "BackOut"},
            "property_stops": [
                {"offset": 1, "rotation": 720, "alpha": 1, "outbound_ease": "ElasticIn"},
                {"offset": 0.5, "rotation": 360, "alpha": 0.5, "outbound_ease": "QuadOut", "scale_y": 2},
                {"offset": 0, "rotation": 0, "alpha": 0, "outbound_ease": "QuadIn"},
            ],
        });
        let stops = parse_stops(&params).unwrap().unwrap();
        let easing = ArrayEasing::parse(&params).unwrap();
        assert!(easing.eases_distribution(&stops));
        for (input, output, rotation, scale_y) in [(0.25, 0.125, 90.0, 1.25), (0.75, 0.875, 630.0, 1.25)] {
            assert_eq!(easing.eased_position(&stops, input), output);
            let props = easing.properties(&stops, input as f32);
            assert_eq!((props.alpha, props.rotation, props.scale[1]), (output as f32, rotation, scale_y));
        }
        assert_eq!(easing.eased_position(&stops, 0.5), 0.5);
        let mut inset = stops;
        inset[2].offset = 0.875;
        assert_eq!(easing.eased_position(&inset, 0.9375), 0.9375, "the terminal margin uses Linear");
        assert_eq!(easing.properties(&inset, 0.9375).rotation, 720.0);
        let global = ArrayEasing { mode: ArrayEaseMode::Global, ..easing };
        assert!(global.eased_position(&inset, 0.5) > 1.0);
    }

    #[test]
    fn array_easing_bakes_the_same_path_as_the_position_preview() {
        for mode in ["linear", "radial"] {
            for closed in [false, true] {
                let mut s = radial_source();
                let base = s.doc.clone();
                let source = s.selection.clone();
                let mut params = json!({
                    "mode": mode, "count": 9, "stacking": "front_to_back",
                    "start_x": 20, "start_y": 40, "dx": 12, "dy": 3,
                    "center_x": 140, "center_y": 100, "radius": 75, "start_angle": 20,
                    "closed_loop": closed, "sweep_angle": -720, "spiral_offset": -20, "drift_x": 35, "drift_y": -10,
                    "easing": {"mode": "global", "preset": "BackOut", "transform": false, "colour_effect": false},
                    "property_stops": [{"offset": 0.0, "alpha": 1}, {"offset": 0.5, "alpha": 0.5}, {"offset": 1, "alpha": 0}],
                });
                s.preview_array(&params).unwrap();
                let generation = s.array_translation_generation().unwrap();
                if mode == "linear" { params["dx"] = json!(20); } else { params["center_x"] = json!(150); }
                s.preview_array(&params).unwrap();
                assert_eq!(s.array_translation_generation(), Some(generation));

                let mut expected = Session::new(base);
                expected.selection = source;
                expected.execute("array.create", &params).unwrap();
                assert!(s.doc == expected.doc && s.selection == expected.selection);
                let copies = s.selected_elements();
                let t = 5.0 / if mode == "radial" && closed { 9.0 } else { 8.0 };
                let position = f64::from(Easing::BackOut.apply(t));
                assert!(position > 1.0);
                let at = if mode == "radial" {
                    parse_radial(&params, Vec2::ZERO).unwrap().point(position)
                } else {
                    Vec2::new(20.0 + 20.0 * (8.0 * position) as f32, 40.0 + 3.0 * (8.0 * position) as f32)
                };
                let copy = &copies[copies.len() - 1 - 5].1;
                assert_eq!(copy.transform.translation(), at);
                assert!((copy.color.alpha() - (1.0 - t)).abs() < 1e-6);
                s.finish_array_preview(true);
                let (_, recorded) = s.history.steps().last().unwrap();
                assert_eq!(recorded.unwrap(), &[crate::history::Recorded::new("array.create", &params)]);
            }
        }
    }

    #[test]
    fn array_easing_validates_parameters_and_handles_one_instance() {
        let mut s = radial_source();
        let original = s.doc.clone();
        for invalid in [
            json!([]), json!({"preset": "unknown"}), json!({"preset": "smooth"}), json!({"preset": {"classic": 50}}),
            json!({"mode": false}), json!({"distribution": 1}),
        ] {
            assert!(s.preview_array(&json!({"easing": invalid})).is_err());
            assert!(s.doc == original && !s.array_preview_active());
        }
        let params = json!({"mode": "radial", "count": 1, "center_x": 50, "center_y": 20, "radius": 20,
            "start_angle": 0, "sweep_angle": -1080, "closed_loop": false, "spiral_offset": -120, "drift_x": -30, "rotate": true,
            "easing": {"mode": "per_segment", "preset": "BackOut"},
            "property_stops": [{"offset": 0.0, "scale_x": 2, "alpha": 0.5}, {"offset": 1.0, "alpha": 0, "rotation": 720}],
        });
        s.execute("array.create", &params).unwrap();
        let copies = s.selected_elements();
        assert_eq!(copies.len(), 1);
        assert_eq!(copies[0].1.transform.translation(), Vec2::new(70.0, 20.0));
        assert_eq!(copies[0].1.transform.m[0], 2.0);
        assert_eq!(copies[0].1.color.alpha(), 0.5);
    }

    #[test]
    fn array_segment_easing_keeps_spiral_geometry_and_composes_with_instance_rotation() {
        for distribution in [false, true] {
            let mut s = radial_source();
            let params = json!({"mode": "radial", "count": 5, "center_x": 75, "center_y": 250, "radius": 120,
                "start_angle": 20, "sweep_angle": -720, "closed_loop": false, "spiral_offset": -30,
                "drift_x": 60, "drift_y": -16, "rotate": true,
                "easing": {"mode": "per_segment", "preset": "QuadIn", "distribution": distribution, "colour_effect": false},
                "property_stops": [
                    {"offset": 0, "rotation": 0, "alpha": 1},
                    {"offset": 0.5, "rotation": 360, "alpha": 0.5, "scale_x": 2},
                    {"offset": 1, "rotation": 720, "alpha": 0},
                ],
            });
            let path = parse_radial(&params, Vec2::ZERO).unwrap();
            s.execute("array.create", &params).unwrap();
            let copies = s.selected_elements();
            let at = if distribution { 0.125 } else { 0.25 };
            let xf = copies[1].1.transform;
            assert_eq!(xf.translation(), path.point(at));
            let (sin, cos) = (90.0_f32 - 720.0 * at as f32).to_radians().sin_cos();
            assert!(xf.apply_vector(Vec2::new(1.0, 0.0)).distance(Vec2::new(cos, sin) * 1.25) < 1e-5);
            assert_eq!(copies[1].1.color.alpha(), 0.75);
            assert_eq!(copies[2].1.transform.translation(), path.point(0.5));
            assert_eq!(copies[4].1.transform.translation(), path.point(1.0));
        }
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
        let a = l.place(Affine::IDENTITY, 0, 10, None);
        let b = l.place(Affine::IDENTITY, 9, 10, None);
        assert_eq!(a.translation().x, 12.0);
        assert_eq!(b.translation().x, 102.0);
        assert_eq!(l.progress(0, 10), 0.0);
        assert_eq!(l.progress(9, 10), 1.0);
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
