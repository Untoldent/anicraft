//! Baked symbol arrays. This command is the engine entry point for a future Array panel.
//!
//! A count of N produces N total instances, including the original (ID preserved).
//! The first instance moves to the requested start location. Copies are ordinary
//! symbol instances, not a new document object, so existing editors and exports work.
//! The UI's live preview can repeatedly execute the same command within a gesture.

use crate::params::{opt_f32, opt_str, opt_u32};
use crate::{CommandSpec, EngineError, Result, Session};
use anicraft_doc::{Element, ElementId, ElementKind};
use anicraft_geom::Affine;
use serde_json::{Value, json};
use std::sync::Arc;

const MAX_COPIES: u32 = 500;
const MAX_STOPS: usize = 15;

/// One property snapshot on the array's 0..1 progression.
#[derive(Clone, Copy, Debug, PartialEq)]
struct AlphaStop {
    offset: f32,
    alpha: f32,
}

pub fn specs() -> Vec<CommandSpec> {
    vec![CommandSpec {
        id: "array.create",
        label: "Create Array",
        menu: "",
        shortcut: None,
        params: "{mode: linear|radial, count: 1..500 (includes original), dx?, dy?, start_x?, start_y? (linear), center_x?, center_y?, radius?, start_angle?, sweep_angle?, rotate? (radial), stacking?: back_to_front|front_to_back, alpha_stops?: [{offset: 0..1, alpha: 0..1}, ...] (2..15)}. Requires exactly one selected symbol instance on the active layer. Moves the original to position 1, bakes the copies and selects them all.",
        mutates: true,
        run: create,
    }]
}

fn parse_stops(p: &Value) -> Result<Option<Vec<AlphaStop>>> {
    let Some(v) = p.get("alpha_stops") else { return Ok(None) };
    let stops = v.as_array().ok_or_else(|| EngineError::Param("alpha_stops must be an array".into()))?;
    if !(2..=MAX_STOPS).contains(&stops.len()) {
        return Err(EngineError::Param(format!("alpha_stops needs 2..={MAX_STOPS} stops")));
    }
    let mut out = Vec::with_capacity(stops.len());
    for v in stops {
        let number = |key: &str| -> Result<f32> {
            let n = v.get(key).and_then(Value::as_f64).ok_or_else(|| EngineError::Param(format!("alpha_stops entries need numeric '{key}'")))?;
            if !n.is_finite() || !(0.0..=1.0).contains(&n) {
                return Err(EngineError::Param(format!("alpha_stops '{key}' must be in 0..=1")));
            }
            Ok(n as f32)
        };
        out.push(AlphaStop { offset: number("offset")?, alpha: number("alpha")? });
    }
    out.sort_by(|a, b| a.offset.total_cmp(&b.offset));
    Ok(Some(out))
}

/// Hold the endpoint value outside the first or last stop, as gradients do.
fn alpha_at(stops: &[AlphaStop], t: f32) -> f32 {
    let Some(first) = stops.first() else { return 1.0 };
    if t <= first.offset { return first.alpha; }
    for pair in stops.windows(2) {
        let [a, b] = pair else { continue };
        if t <= b.offset {
            let span = b.offset - a.offset;
            return if span <= f32::EPSILON { b.alpha } else {
                a.alpha + (b.alpha - a.alpha) * ((t - a.offset) / span).clamp(0.0, 1.0)
            };
        }
    }
    stops.last().map_or(first.alpha, |s| s.alpha)
}

#[derive(Clone, Copy, Debug)]
enum Layout {
    Linear { start_x: f32, start_y: f32, dx: f32, dy: f32 },
    Radial { cx: f32, cy: f32, radius: f32, start: f32, sweep: f32, rotate: bool },
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
            Layout::Radial { cx, cy, radius, start, sweep, rotate } => {
                // A complete revolution has N unique places, not N-1 and a duplicate at 360.
                let closed = (sweep.abs() - std::f32::consts::TAU).abs() < 1e-5;
                let divisor = if closed { count } else { count.saturating_sub(1).max(1) };
                let t = index as f32 / divisor as f32;
                let angle = start + sweep * t;
                let mut m = source;
                let (sin, cos) = angle.sin_cos();
                let (x, y) = (cx + radius * cos, cy + radius * sin);
                m.m[4] = x;
                m.m[5] = y;
                if rotate {
                    // Change the instance's own orientation while keeping its registration
                    // point at (x, y). Do not rotate its translation around stage origin.
                    let turn = Affine::translate(x, y)
                        .mul(&Affine::rotate(sweep * t))
                        .mul(&Affine::translate(-x, -y));
                    m = turn.mul(&m);
                }
                (m, t)
            }
        }
    }
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
    let chosen = s.selection.on(s.layer);
    let [id] = chosen.as_slice() else {
        return Err(EngineError::State("select exactly one symbol instance on the active layer".into()));
    };
    let original = s.editable_keyframe()?.elements.iter().find(|e| e.id == *id)
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
        "radial" => {
            let cx = opt_f32(p, "center_x")?.ok_or_else(|| EngineError::Param("radial requires center_x".into()))?;
            let cy = opt_f32(p, "center_y")?.ok_or_else(|| EngineError::Param("radial requires center_y".into()))?;
            let default_radius = (original_x - cx).hypot(original_y - cy);
            let radius = opt_f32(p, "radius")?.unwrap_or(default_radius);
            if !radius.is_finite() || radius < 0.0 {
                return Err(EngineError::Param("radius must be finite and >= 0".into()));
            }
            let start = opt_f32(p, "start_angle")?.unwrap_or((original_y - cy).atan2(original_x - cx).to_degrees()).to_radians();
            let sweep = opt_f32(p, "sweep_angle")?.unwrap_or(360.0).to_radians();
            Layout::Radial {
                cx, cy, radius, start, sweep,
                rotate: crate::params::opt_bool(p, "rotate")?.unwrap_or(false),
            }
        }
        other => return Err(EngineError::Param(format!("unknown array mode '{other}' (linear, radial)"))),
    };

    let mut copies: Vec<Arc<Element>> = Vec::with_capacity(count as usize);
    let mut ids: Vec<ElementId> = Vec::with_capacity(count as usize);
    for i in 0..count {
        let (transform, t) = layout.place(original.transform, i, count);
        if !transform.is_finite() {
            return Err(EngineError::Param("array positions exceeded finite coordinate range".into()));
        }
        let mut el = original.clone();
        // Instance #1 is the original and keeps its ID.
        if i > 0 { el.id = ElementId(s.doc.alloc_id()); }
        el.transform = transform;
        if let Some(stops) = stops.as_deref() {
            el.color = el.color.with_alpha(alpha_at(stops, t));
        }
        ids.push(el.id);
        copies.push(Arc::new(el));
    }

    // Preserve the source's position relative to unrelated artwork. Copies stack beside it.
    // Only the relative order among the array copies is controlled by "stacking".
    if front_to_back { copies.reverse(); }
    let kf = s.editable_keyframe()?;
    let at = kf.elements.iter().position(|e| e.id == *id)
        .ok_or_else(|| EngineError::State("original disappeared from the keyframe".into()))?;
    kf.elements.remove(at);
    kf.elements.splice(at..at, copies);
    s.select_only(ids.iter().copied());
    Ok(json!({"count": count, "mode": mode, "instances": ids.iter().map(|id| id.0).collect::<Vec<_>>()}))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn st(p: f32, alpha: f32) -> AlphaStop { AlphaStop { offset: p, alpha } }

    #[test]
    fn independent_stops_can_fade_out_then_back_in() {
        let stops = [st(0.0, 1.0), st(0.5, 0.0), st(1.0, 1.0)];
        assert!((alpha_at(&stops, 0.25) - 0.5).abs() < 1e-6);
        assert!((alpha_at(&stops, 0.75) - 0.5).abs() < 1e-6);
        assert_eq!(alpha_at(&stops, 0.5), 0.0);
    }

    #[test]
    fn a_closed_radial_array_does_not_duplicate_its_first_position() {
        let l = Layout::Radial { cx: 100.0, cy: 100.0, radius: 50.0, start: 0.0, sweep: std::f32::consts::TAU, rotate: false };
        let (a, ta) = l.place(Affine::IDENTITY, 0, 12);
        let (b, tb) = l.place(Affine::IDENTITY, 11, 12);
        assert_eq!(ta, 0.0);
        assert!(tb < 1.0);
        assert!((a.m[4] - b.m[4]).abs() > 1.0);
        assert!((b.m[4] - 100.0).abs() > 1.0);
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
    }
}
