//! Array's stage controls and tile coverage for Linear drags.

use crate::app::App;
use crate::theme::{self, T, Type};
use anicraft_engine::StateId;
use anicraft_engine::doc::{DisplayList, Element, LayerId};
use anicraft_engine::geom::{Affine, Rect as ArtRect, Vec2, segment_distance};
use anicraft_engine::render_service::{FrameJob, frame_key};
use egui::{CursorIcon, Id, Painter, PointerButton, Pos2, Rect, Sense, Stroke, Ui, pos2, vec2};
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::sync::Arc;

const HANDLE_RADIUS: f32 = 6.0;
const HANDLE_HIT: f32 = 12.0;
const BAR_HIT: f32 = 9.0;

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Endpoints {
    pub start: [f32; 2],
    pub end: [f32; 2],
}

impl Endpoints {
    pub fn extent(self) -> [f32; 2] {
        [self.end[0] - self.start[0], self.end[1] - self.start[1]]
    }

    pub fn spacing(self, count: u32) -> Option<[f32; 2]> {
        (count > 1).then(|| self.extent().map(|d| d / (count - 1) as f32))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Grip {
    Start,
    End,
    Bar,
}

#[derive(Clone, Copy)]
pub(crate) struct GizmoDrag {
    grip: Grip,
    press: Vec2,
    original: Endpoints,
}

impl GizmoDrag {
    fn at(self, pointer: Vec2) -> Option<Endpoints> {
        let delta = pointer - self.press;
        let shift = |[x, y]: [f32; 2]| [x + delta.x, y + delta.y];
        let mut endpoints = self.original;
        if self.grip != Grip::End {
            endpoints.start = shift(endpoints.start);
        }
        if self.grip != Grip::Start {
            endpoints.end = shift(endpoints.end);
        }
        endpoints.start.into_iter().chain(endpoints.end).all(f32::is_finite).then_some(endpoints)
    }
}

pub(crate) struct Overlay {
    start: Pos2,
    end: Pos2,
    hot: Option<Grip>,
}

fn as_vec(p: Pos2) -> Vec2 {
    Vec2::new(p.x, p.y)
}

fn registration(element: &Element, projection: Affine) -> Option<Pos2> {
    let p = projection.apply(element.transform.apply(element.pivot().unwrap_or(Vec2::ZERO)));
    p.is_finite().then_some(pos2(p.x, p.y))
}

pub(crate) fn paint_registrations(painter: &Painter, projection: Affine, instances: &[(LayerId, Arc<Element>)]) {
    let clip = painter.clip_rect().expand(6.0);
    for (_, instance) in instances {
        if let Some(p) = registration(instance, projection).filter(|p| clip.contains(*p)) {
            theme::registration_mark(painter, p);
        }
    }
}

/// While only Linear translations change, the surrounding artwork and the copy's footprint
/// are stable. Give a tile a new key only when one of its intersecting instances changes.
#[derive(Clone, Copy)]
pub(crate) struct TileBounds {
    pub generation: StateId,
    relative: Option<ArtRect>,
}

impl TileBounds {
    pub fn new(generation: StateId, list: &DisplayList, instances: &[(LayerId, Arc<Element>)]) -> Self {
        let positions: HashMap<_, _> = instances.iter().map(|(_, el)| (el.id, el.transform.translation())).collect();
        let mut bounds = Self { generation, relative: None };
        // Filters can extend beyond path bounds and a moved mask can change other artwork.
        // Keep the full render path in those scenes.
        if list.groups.iter().any(|g| !g.filters.is_empty()) { return bounds; }
        let mut footprint = ArtRect::EMPTY;
        for item in &list.items {
            let Some(at) = positions.get(&item.element) else { continue };
            if item.masks.is_some() { return bounds; }
            let reach = item.stroke.as_ref().map_or(0.0, anicraft_engine::doc::Stroke::reach);
            let xf = Affine::translate(-at.x, -at.y).mul(&item.transform);
            footprint = footprint.union(&item.path.bounds().inflate(reach + 1.0).transformed(&xf));
        }
        bounds.relative = Some(footprint);
        bounds
    }

    pub fn key(&self, job: &FrameJob, tile: [f32; 4], pixel: f32, dim: bool, instances: &[(LayerId, Arc<Element>)]) -> u64 {
        let Some(footprint) = self.relative else { return job.key };
        let [x0, y0, x1, y1] = tile;
        let tile = ArtRect::new(x0, y0, x1, y1).inflate(2.0 * pixel);
        let mut hash = std::collections::hash_map::DefaultHasher::new();
        ("array_tile", frame_key(self.generation, job.frame, &job.view, None), job.raster, dim).hash(&mut hash);
        if !footprint.is_empty() {
            for (_, el) in instances {
                let at = el.transform.translation();
                let bounds = ArtRect { min: footprint.min + at, max: footprint.max + at };
                if !tile.intersect(&bounds).is_empty() {
                    (el.id, at.x.to_bits(), at.y.to_bits()).hash(&mut hash);
                }
            }
        }
        hash.finish()
    }
}

impl Overlay {
    fn new(endpoints: Endpoints, to_screen: Affine) -> Option<Self> {
        let project = |[x, y]: [f32; 2]| {
            let p = to_screen.apply(Vec2::new(x, y));
            p.is_finite().then_some(pos2(p.x, p.y))
        };
        Some(Self { start: project(endpoints.start)?, end: project(endpoints.end)?, hot: None })
    }

    fn coincident(&self) -> bool {
        self.start.distance_sq(self.end) < 1.0
    }

    fn hit(&self, p: Pos2) -> Option<Grip> {
        let start = p.distance(self.start);
        let end = p.distance(self.end);
        if self.coincident() && start <= HANDLE_HIT {
            // Both endpoints remain reachable at the same point: the centre and the outer ring.
            return Some(if start < HANDLE_RADIUS { Grip::Start } else { Grip::End });
        }
        if start <= HANDLE_HIT && start <= end {
            Some(Grip::Start)
        } else if end <= HANDLE_HIT {
            Some(Grip::End)
        } else if segment_distance(as_vec(p), as_vec(self.start), as_vec(self.end)) <= BAR_HIT {
            Some(Grip::Bar)
        } else {
            None
        }
    }

    pub(crate) fn paint(&self, painter: &Painter) {
        let line = if self.hot == Some(Grip::Bar) { T.accent } else { T.selection };
        painter.line_segment([self.start, self.end], Stroke::new(1.5, line));
        let handle = |grip, at, radius, fill| {
            let edge = if self.hot == Some(grip) { T.accent } else { T.selection };
            painter.circle(at, radius, fill, Stroke::new(1.5, edge));
        };
        let end_radius = if self.coincident() { HANDLE_RADIUS + 4.0 } else { HANDLE_RADIUS };
        handle(Grip::End, self.end, end_radius, T.panel);
        handle(Grip::Start, self.start, HANDLE_RADIUS, T.selection);
        for (label, at, above) in [(tr!("Start"), self.start, true), (tr!("End"), self.end, false)] {
            let galley = painter.layout_no_wrap(label.to_owned(), theme::font(Type::Caption), T.text);
            let y = if above { at.y - end_radius - galley.size().y - 6.0 } else { at.y + end_radius + 6.0 };
            let pos = pos2(at.x - galley.size().x * 0.5, y);
            painter.rect_filled(Rect::from_min_size(pos, galley.size()).expand2(vec2(4.0, 2.0)), T.rc(), T.panel);
            painter.galley(pos, galley, T.text);
        }
    }
}

/// Update before the stage submits tiles, so the picture and handles use the same geometry.
pub(crate) fn input(app: &mut App, ui: &mut Ui, canvas: Rect, to_screen: Option<Affine>) -> Option<Overlay> {
    let (Some(endpoints), Some(to_screen)) = (app.array_panel.gizmo_endpoints(), to_screen) else {
        app.array_panel.gizmo_drag = None;
        return None;
    };
    let mut overlay = Overlay::new(endpoints, to_screen)?;
    if !app.array_panel.gizmo_editable()
        || app.modal_open()
        || app.palette.is_some()
        || egui::Popup::is_any_open(ui.ctx())
        || app.dock_view.drag.is_some()
    {
        app.array_panel.gizmo_drag = None;
        return Some(overlay);
    }
    let to_stage = to_screen.inverse()?;
    // egui hits rectangles; resolve the actual circles and segment at the original press.
    let response = ui.interact(canvas, Id::new("array_linear_gizmo"), Sense::drag());
    if response.drag_started_by(PointerButton::Primary)
        && let Some(press) = ui.input(|i| i.pointer.press_origin())
        && let Some(grip) = overlay.hit(press)
    {
        app.array_panel.gizmo_drag =
            Some(GizmoDrag { grip, press: to_stage.apply(as_vec(press)), original: endpoints });
    }
    if let Some(drag) = app.array_panel.gizmo_drag {
        if let Some(p) = response.interact_pointer_pos()
            && (response.dragged_by(PointerButton::Primary) || response.drag_stopped_by(PointerButton::Primary))
            && let Some(moved) = drag.at(to_stage.apply(as_vec(p)))
            && moved != endpoints
        {
            app.array_panel.set_endpoints(&mut app.s, moved);
            overlay = Overlay::new(moved, to_screen)?;
            ui.ctx().request_repaint();
        }
        if !ui.input(|i| i.pointer.primary_down()) {
            app.array_panel.gizmo_drag = None;
        } else {
            overlay.hot = Some(drag.grip);
            ui.ctx().set_cursor_icon(CursorIcon::Grabbing);
            return Some(overlay);
        }
    }
    overlay.hot = response.hover_pos().and_then(|p| overlay.hit(p));
    if overlay.hot.is_some() {
        ui.ctx().set_cursor_icon(CursorIcon::Grab);
    }
    Some(overlay)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn coincident_endpoints_can_be_grabbed_independently() {
        let p = [90.0, 60.0];
        let overlay = Overlay::new(Endpoints { start: p, end: p }, Affine::IDENTITY).unwrap();
        assert_eq!(overlay.hit(pos2(90.0, 60.0)), Some(Grip::Start));
        assert_eq!(overlay.hit(pos2(98.0, 60.0)), Some(Grip::End));
        assert_eq!(overlay.hit(pos2(110.0, 60.0)), None);
    }

    #[test]
    fn linear_tiles_follow_the_copies_and_clear_the_area_they_left() {
        use anicraft_engine::render::View;
        use anicraft_engine::render_service::{make_job, render_job};
        use serde_json::json;
        let mut s = anicraft_engine::Session::default();
        s.execute("draw.rect", &json!({"x": 0, "y": 0, "w": 20, "h": 20, "fill": "#ff0000", "stroke": null})).unwrap();
        s.execute("modify.convert_to_symbol", &json!({"name": "Source"})).unwrap();
        let mut p = json!({"mode": "linear", "count": 2, "start_x": 50, "start_y": 25, "dx": 20, "dy": 0});
        s.preview_array(&p).unwrap();
        let generation = s.linear_array_generation().unwrap();
        let footprint = TileBounds::new(generation, &s.display_list(), &s.selected_elements());
        let tile = |session: &anicraft_engine::Session, left: f32| {
            let view = View { width: 100, height: 60, transform: Affine::translate(-left, 0.0), background: None };
            let mut job = make_job(session.state, &session.doc, 0, view, None);
            job.key = footprint.key(&job, [left, 0.0, left + 100.0, 60.0], 1.0, false, &session.selected_elements());
            job
        };
        let old = [tile(&s, 0.0), tile(&s, 300.0), tile(&s, 1300.0)];
        p["start_x"] = json!(1350);
        s.preview_array(&p).unwrap();
        let new = [tile(&s, 0.0), tile(&s, 300.0), tile(&s, 1300.0)];
        assert_eq!(s.linear_array_generation(), Some(generation));
        assert_ne!(old[0].key, new[0].key);
        assert_eq!(old[1].key, new[1].key);
        assert_ne!(old[2].key, new[2].key);
        for i in [0, 2] {
            assert_ne!(render_job(&old[i]).rgba_premul, render_job(&new[i]).rgba_premul);
        }
        assert_eq!(render_job(&old[1]).rgba_premul, render_job(&new[1]).rgba_premul);
    }
}
