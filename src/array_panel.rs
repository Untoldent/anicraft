//! The Array panel retains unfinished settings for recovery after cancellation.

use crate::a11y::Named;
use crate::app::{App, Drag};
use crate::array_gizmo::{Endpoints, Geometry, GizmoDrag, RADIAL_FIELD_LIMIT};
use crate::colour_panel::PropertyStop;
use crate::dock::{Panel, Place};
use crate::theme::{FORM_LABEL_DOCK, Section, T, Type, form_row, section, text, with_type};
use anicraft_engine::{ArrayEaseMode, ArrayEasing, ArrayPropertyStop, CLOSED_SWEEP_LIMIT, RadialDistribution as Radial, Session};
use anicraft_engine::doc::{Easing, ElementId, ElementKind, LayerId};
use egui::{Id, Key, Modifiers, Ui};
use serde_json::{Value, json};
use std::collections::VecDeque;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mode {
    Linear,
    Radial,
}

#[derive(Clone, Debug, PartialEq)]
struct Settings {
    default_origin: [f32; 2],
    count: u32,
    linear: Endpoints,
    radial: Radial,
    backwards: bool,
    stops: Vec<PropertyStop>,
    easing: ArrayEasing,
}

impl Settings {
    fn at([x, y]: [f32; 2]) -> Self {
        let radius = 120.0;
        Self {
            default_origin: [x, y],
            count: 10,
            // Keep the original 36 px default spacing across nine intervals.
            linear: Endpoints { start: [x, y], end: [x + 324.0, y] },
            radial: Radial {
                centre: [x - radius, y], radius, start: 0.0, sweep: CLOSED_SWEEP_LIMIT,
                closed_loop: true, spiral_offset: 0.0, centre_drift: [0.0; 2], rotate: false,
            },
            backwards: false,
            stops: vec![PropertyStop::new(0, 0.0, 1.0), PropertyStop::new(1, 1.0, 1.0)],
            easing: ArrayEasing::default(),
        }
    }

    fn params(&self, mode: Mode) -> Value {
        let property_stops: Vec<Value> = self
            .stops
            .iter()
            .map(|s| {
                json!({
                    "offset": s.value.offset, "alpha": s.value.alpha,
                    "scale_x": s.value.scale[0], "scale_y": s.value.scale[1], "rotation": s.value.rotation,
                    "outbound_ease": anicraft_engine::params::easing_json(s.value.outbound.as_ref().unwrap_or(&Easing::Linear)),
                })
            })
            .collect();
        let mut params = match mode {
            Mode::Linear => {
                let [dx, dy] = self.linear.spacing(self.count).unwrap_or_default();
                json!({
                    "mode": "linear", "count": self.count,
                    "start_x": self.linear.start[0], "start_y": self.linear.start[1],
                    "dx": dx, "dy": dy,
                })
            }
            Mode::Radial => json!({
                "mode": "radial", "count": self.count,
                "center_x": self.radial.centre[0], "center_y": self.radial.centre[1],
                "radius": self.radial.radius, "start_angle": self.radial.start,
                "sweep_angle": self.radial.sweep, "closed_loop": self.radial.closed_loop,
                "spiral_offset": self.radial.spiral_offset,
                "drift_x": self.radial.centre_drift[0], "drift_y": self.radial.centre_drift[1],
                "rotate": self.radial.rotate,
            }),
        };
        params["stacking"] = json!(if self.backwards { "front_to_back" } else { "back_to_front" });
        params["property_stops"] = json!(property_stops);
        params["easing"] = json!({
            "mode": self.easing.mode.key(), "preset": anicraft_engine::params::easing_json(&self.easing.preset),
            "distribution": self.easing.distribution, "transform": self.easing.transform,
            "colour_effect": self.easing.colour_effect,
        });
        params
    }
}

struct Active {
    mode: Mode,
    /// Where defaults are anchored if settings are reset during this preview.
    origin: [f32; 2],
    history: EditHistory,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum EditGesture {
    Pointer(egui::ViewportId, Id),
    Text(egui::ViewportId, Id),
}

struct Configuration {
    mode: Mode,
    settings: Settings,
    selected_stop: Option<usize>,
}

#[derive(Default)]
struct EditHistory {
    undo: VecDeque<Configuration>,
    redo: Vec<Configuration>,
    pending: Option<(EditGesture, Configuration)>,
}

impl EditHistory {
    fn record(&mut self, before: Configuration) {
        self.redo.clear();
        self.undo.push_back(before);
        if self.undo.len() > anicraft_engine::history::MAX_STEPS {
            self.undo.pop_front();
        }
    }

    fn settle(&mut self, mode: Mode, settings: &Settings) {
        if let Some((_, before)) = self.pending.take()
            && (before.mode != mode || &before.settings != settings)
        {
            self.record(before);
        }
    }
}

pub struct State {
    active: Option<Active>,
    settings: Option<Settings>,
    draft_source: Option<(LayerId, ElementId)>,
    error: Option<String>,
    reset_pending: bool,
    stop_editor_epoch: u64,
    selected_stop: Option<usize>,
    show_gizmo: bool,
    dim_stage: bool,
    pub(crate) gizmo_drag: Option<GizmoDrag>,
}

impl Default for State {
    fn default() -> Self {
        Self {
            active: None,
            settings: None,
            draft_source: None,
            error: None,
            reset_pending: false,
            stop_editor_epoch: 0,
            selected_stop: None,
            show_gizmo: true,
            dim_stage: true,
            gizmo_drag: None,
        }
    }
}

fn source_origin(s: &Session) -> Option<[f32; 2]> {
    if s.selection.len() != 1 {
        return None;
    }
    let selected = s.selected_elements();
    let [(layer, element)] = selected.as_slice() else { return None };
    (*layer == s.layer && matches!(element.kind, ElementKind::Symbol { .. }))
        .then(|| [element.transform.m[4], element.transform.m[5]])
}

impl State {
    fn mode(&self) -> Option<Mode> {
        self.active.as_ref().map(|active| active.mode)
    }

    fn select_mode(&mut self, s: &mut Session, mode: Option<Mode>) {
        self.check_draft(s);
        if mode == self.mode() {
            return;
        }
        self.gizmo_drag = None;
        let Some(mode) = mode else {
            self.finish(s, false);
            return;
        };
        let before = self.before_edit(None);
        if let Some(active) = &mut self.active {
            active.mode = mode;
            self.record_edit(before, None);
            self.refresh(s);
        } else if let Some(origin) = source_origin(s) {
            let settings = self.settings.clone().unwrap_or_else(|| Settings::at(origin));
            match s.preview_array(&settings.params(mode)) {
                Ok(()) => {
                    self.active = Some(Active { mode, origin, history: EditHistory::default() });
                    self.selected_stop = self.selected_stop.filter(|id| settings.stops.iter().any(|stop| stop.marker_id() == *id))
                        .or_else(|| settings.stops.first().map(PropertyStop::marker_id));
                    self.settings = Some(settings);
                    self.draft_source = None;
                    self.error = None;
                }
                Err(error) => self.error = Some(error.to_string()),
            }
        }
    }

    fn refresh(&mut self, s: &mut Session) {
        if let (Some(mode), Some(settings)) = (self.mode(), &self.settings) {
            self.error = s.preview_array(&settings.params(mode)).err().map(|e| e.to_string());
        }
    }

    fn finish(&mut self, s: &mut Session, commit: bool) {
        if commit && self.error.is_some() {
            return;
        }
        s.finish_array_preview(commit);
        let was_active = self.active.take().is_some();
        if commit {
            self.discard_draft();
        } else if was_active {
            // The engine just restored the original, before any preview copies were selected.
            self.draft_source = s.selection.first();
        }
        self.error = None;
        self.reset_pending = false;
        self.gizmo_drag = None;
    }

    pub(crate) fn discard_draft(&mut self) {
        self.settings = None;
        self.draft_source = None;
        self.reset_pending = false;
        self.selected_stop = None;
        self.stop_editor_epoch = self.stop_editor_epoch.wrapping_add(1);
    }

    pub(crate) fn check_draft(&mut self, s: &Session) {
        if let Some(source) = self.draft_source
            && (s.selection.len() != 1 || s.selection.first() != Some(source))
        {
            self.discard_draft();
        }
    }

    fn customized(&self) -> bool {
        self.settings.as_ref().is_some_and(|settings| *settings != Settings::at(settings.default_origin))
    }

    fn reset(&mut self, s: &mut Session) {
        let before = self.before_edit(None);
        self.discard_draft();
        self.settings = self.active.as_ref().map(|active| Settings::at(active.origin));
        self.selected_stop = self.settings.as_ref().and_then(|settings| settings.stops.first()).map(PropertyStop::marker_id);
        self.gizmo_drag = None;
        self.record_edit(before, None);
        self.refresh(s);
    }

    // Keep only the settings and the stop pick. The engine still has one open document gesture.
    fn before_edit(&self, gesture: Option<EditGesture>) -> Option<Configuration> {
        let active = self.active.as_ref()?;
        if active.history.pending.as_ref().is_some_and(|(id, _)| Some(*id) == gesture) {
            return None;
        }
        Some(Configuration { mode: active.mode, settings: self.settings.clone()?, selected_stop: self.selected_stop })
    }

    fn differs(&self, before: &Configuration) -> bool {
        self.mode() != Some(before.mode) || self.settings.as_ref() != Some(&before.settings)
    }

    fn record_edit(&mut self, before: Option<Configuration>, gesture: Option<EditGesture>) {
        let Some(before) = before.filter(|before| self.differs(before)) else { return };
        let Some(active) = &mut self.active else { return };
        // A different edit may arrive before the previous gesture ended.
        active.history.settle(before.mode, &before.settings);
        if let Some(id) = gesture {
            active.history.pending = Some((id, before));
        } else {
            active.history.record(before);
        }
    }

    pub(crate) fn finish_edit(&mut self, ctx: &egui::Context) {
        let (Some(active), Some(settings)) = (&mut self.active, &self.settings) else { return };
        if active.history.pending.as_ref().is_some_and(|(gesture, _)| match *gesture {
            EditGesture::Pointer(viewport, id) => viewport == ctx.viewport_id() && ctx.dragged_id() != Some(id),
            EditGesture::Text(viewport, id) => viewport == ctx.viewport_id() && focused_text(ctx) != Some(id),
        }) {
            active.history.settle(active.mode, settings);
        }
    }

    fn cancel_text_edit(&mut self, s: &mut Session, gesture: EditGesture) {
        let Some(active) = &mut self.active else { return };
        if active.history.pending.as_ref().is_some_and(|(owner, _)| *owner == gesture)
            && let Some((_, before)) = active.history.pending.take()
        {
            self.settings = Some(before.settings);
            self.selected_stop = before.selected_stop;
            self.refresh(s);
        }
    }

    pub(crate) fn can_step(&self, redo: bool) -> bool {
        let Some(active) = &self.active else { return false };
        let changed_edit = active.history.pending.as_ref().is_some_and(|(_, before)| self.differs(before));
        if redo { !changed_edit && !active.history.redo.is_empty() }
        else { changed_edit || !active.history.undo.is_empty() }
    }

    pub(crate) fn step(&mut self, s: &mut Session, redo: bool) -> bool {
        if !self.can_step(redo) { return false; }
        let (Some(active), Some(settings)) = (&mut self.active, &mut self.settings) else { return false };
        let history = &mut active.history;
        history.settle(active.mode, settings);
        let next = if redo { history.redo.pop() } else { history.undo.pop_back() };
        let Some(next) = next else { return false };
        let current = Configuration {
            mode: std::mem::replace(&mut active.mode, next.mode),
            settings: std::mem::replace(settings, next.settings),
            selected_stop: std::mem::replace(&mut self.selected_stop, next.selected_stop),
        };
        if redo { history.undo.push_back(current); } else { history.redo.push(current); }
        self.reset_pending = false;
        self.gizmo_drag = None;
        self.stop_editor_epoch = self.stop_editor_epoch.wrapping_add(1);
        self.refresh(s);
        true
    }

    pub(crate) fn gizmo_geometry(&self) -> Option<Geometry> {
        if !self.show_gizmo { return None; }
        let settings = self.settings.as_ref()?;
        Some(match self.mode()? {
            Mode::Linear => Geometry::Linear(settings.linear),
            Mode::Radial => Geometry::Radial(settings.radial),
        })
    }

    #[cfg(test)]
    fn gizmo_endpoints(&self) -> Option<Endpoints> {
        match self.gizmo_geometry()? { Geometry::Linear(ends) => Some(ends), Geometry::Radial(_) => None }
    }

    pub(crate) fn shows_registrations(&self) -> bool {
        self.show_gizmo && self.active.is_some() && self.error.is_none()
    }

    pub(crate) fn gizmo_editable(&self) -> bool {
        !self.reset_pending
    }

    pub(crate) fn set_gizmo_geometry(&mut self, s: &mut Session, geometry: Geometry, gesture: Option<EditGesture>) {
        if self.gizmo_geometry().is_none_or(|old| old == geometry) { return; }
        let before = self.before_edit(gesture);
        if let Some(settings) = &mut self.settings {
            match geometry {
                Geometry::Linear(ends) => settings.linear = ends,
                Geometry::Radial(radial) => settings.radial = radial,
            }
        }
        self.record_edit(before, gesture);
        self.refresh(s);
    }

    #[cfg(test)]
    fn set_endpoints(&mut self, s: &mut Session, endpoints: Endpoints) {
        self.set_gizmo_geometry(s, Geometry::Linear(endpoints), None);
    }

    pub(crate) fn dim_stage(&self) -> bool {
        self.dim_stage && self.active.is_some() && self.error.is_none()
    }
}

pub(crate) fn blocked(app: &mut App) -> bool {
    if app.s.array_preview_active() {
        app.status_err(tr!("Commit or cancel the Array preview first."));
        true
    } else {
        false
    }
}

pub(crate) fn history_action(id: &str) -> Option<bool> {
    match id {
        "edit.undo" => Some(false),
        "edit.redo" => Some(true),
        _ => None,
    }
}

pub(crate) fn blocked_command(app: &mut App, id: &str) -> bool {
    history_action(id).is_none() && blocked(app)
}

pub(crate) fn history_command(app: &mut App, redo: bool) -> Option<Value> {
    if app.array_panel.step(&mut app.s, redo) {
        let label = tr_noop!("Array edit");
        return Some(if redo { json!({"redone": label}) } else { json!({"undone": label}) });
    }
    app.status_err(if redo { tr!("Nothing to redo in this Array session.") } else { tr!("Nothing to undo in this Array session.") });
    None
}

pub(crate) fn history_shortcut(app: &mut App, ctx: &egui::Context) -> bool {
    if !app.s.array_preview_active() || focused_text(ctx).is_some() || app.modal_open()
        || app.palette.is_some() || egui::Popup::is_any_open(ctx)
    {
        return false;
    }
    let action = ctx.input_mut(|input| {
        // In Array mode this chord is Redo; in the document it keeps its normal binding.
        if input.consume_key(Modifiers::COMMAND | Modifiers::SHIFT, Key::Z) {
            return Some("edit.redo");
        }
        app.shortcuts.iter().find_map(|(mods, key, id)| {
            (history_action(id).is_some() && input.consume_key(*mods, *key)).then_some(*id)
        })
    });
    if let Some(action) = action {
        if history_action(action).is_some_and(|redo| app.array_panel.can_step(redo)) {
            ctx.stop_dragging();
        }
        app.invoke(action);
        ctx.request_repaint_of(egui::ViewportId::ROOT);
        true
    } else {
        false
    }
}

/// Keep the transaction visible in the editor even when a different dock tab is shown.
pub(crate) fn edit_bar(app: &mut App, ui: &mut Ui) {
    ui.horizontal_centered(|ui| {
        ui.label(text(Type::Label, tr!("Array preview")));
        let floating = app.dock.is_floating(Panel::Array);
        if !app.panel_visible(Panel::Array) || floating {
            if ui.button(tr!("Show Array")).clicked() {
                app.show_panel(Panel::Array);
                if crate::dock_ui::own_windows(app, ui.ctx())
                    && let Some(Place::Floating { window, .. }) = app.dock.place(Panel::Array)
                    && let Some(float) = app.dock.floating.get(window)
                {
                    let viewport = egui::ViewportId::from_hash_of(("dock_float", float.id));
                    ui.ctx().send_viewport_cmd_to(viewport, egui::ViewportCommand::Focus);
                }
            }
            finish_buttons(&mut app.array_panel, &mut app.s, ui);
        }
    });
}

fn finish_buttons(state: &mut State, s: &mut Session, ui: &mut Ui) {
    if ui.add_enabled(state.error.is_none() && !state.reset_pending, egui::Button::new(tr!("Commit"))).clicked() {
        state.finish(s, true);
        ui.ctx().request_repaint_of(egui::ViewportId::ROOT);
    }
    if ui.button(tr!("Cancel")).clicked() {
        state.finish(s, false);
        ui.ctx().request_repaint_of(egui::ViewportId::ROOT);
    }
}

pub(crate) fn can_apply_on_quit(app: &App) -> bool {
    app.array_panel.error.is_none()
}

pub(crate) fn finish_for_quit(app: &mut App, apply: bool) {
    app.array_panel.finish(&mut app.s, apply);
    if !app.s.array_preview_active() {
        app.array_quit_pending = false;
        crate::tabs::quit(app);
    }
}

fn text_focus_id(ctx: &egui::Context) -> Id {
    Id::new(("array_text_focus", ctx.viewport_id()))
}

pub(crate) fn remember_text_focus(ctx: &egui::Context) {
    let focus = focused_text(ctx);
    let key = text_focus_id(ctx);
    ctx.data_mut(|d| d.insert_temp(key, focus));
}

fn focused_text(ctx: &egui::Context) -> Option<Id> {
    ctx.memory(|m| m.focused()).filter(|_| ctx.text_edit_focused())
}

pub(crate) fn finish_frame(app: &mut App, ctx: &egui::Context) {
    app.array_panel.finish_edit(ctx);
    app.array_panel.check_draft(&app.s);
    if !app.panel_visible(Panel::Array) {
        app.array_panel.reset_pending = false;
    }
    remember_text_focus(ctx);
}

/// A live preview with property stops and surrounding artwork for screenshot checks.
pub(crate) fn screenshot(app: &mut App, view: &str) {
    let segment_sample = matches!(view, "array-easing-segment" | "array-easing-segment-middle" | "array-easing-segment-end");
    let transform_sample = segment_sample || matches!(view, "array-transform" | "array-transform-radial" | "array-easing-global");
    let open_radial = view.strip_prefix("array-radial-open:").and_then(|setup| {
        let mut values = setup.split(',');
        let sweep = values.next()?.parse::<f32>().ok()?;
        let offset = values.next()?.parse::<f32>().ok()?;
        let mut drift = [0.0; 2];
        if let Some(x) = values.next() {
            drift = [x.parse::<f32>().ok()?, values.next()?.parse::<f32>().ok()?];
        }
        Some((sweep, offset, drift))
    });
    app.perform("file.new", json!({"width": 640, "height": 400}));
    app.run("draw.rect", json!({"x": 72, "y": 72, "w": 125, "h": 60, "fill": "#376b93", "stroke": null}));
    app.run("draw.ellipse", json!({"x": 494, "y": 275, "w": 82, "h": 68, "fill": "#376b93", "stroke": null}));
    let diameter = if open_radial.is_some() { 20 } else { 56 };
    if transform_sample {
        app.run("draw.rect", json!({"x": 92, "y": 172, "w": 34, "h": 15, "fill": "#ce523f", "stroke": null}));
    } else {
        app.run("draw.ellipse", json!({"x": 135, "y": 170, "w": diameter, "h": diameter, "fill": "#ce523f", "stroke": null}));
    }
    app.run("modify.convert_to_symbol", json!({"name": "Array sample"}));
    if matches!(view, "array-polish" | "array-polish-off" | "array-radial")
        && let Some((_, source)) = app.s.selection.first()
    {
        app.run("draw.rect", json!({"x": 253, "y": 160, "w": 125, "h": 112, "fill": "#376b93", "stroke": null}));
        app.s.select_only([source]);
    }
    app.show_panel(Panel::Array);
    if app.shot.is_some() {
        for group in &mut app.dock.right.groups {
            group.folded = !group.tabs.contains(&Panel::Array);
        }
    }
    app.tool = crate::app::Tool::Select;
    let mode = if segment_sample || matches!(view, "array-radial" | "array-transform-radial") || open_radial.is_some() { Mode::Radial } else { Mode::Linear };
    app.array_panel.select_mode(&mut app.s, Some(mode));
    app.array_panel.show_gizmo = !matches!(view, "array-hidden" | "array-polish-off");
    app.array_panel.dim_stage = view != "array-polish-off";
    if let Some(settings) = &mut app.array_panel.settings {
        settings.linear.end[1] += 50.0;
        if mode == Mode::Radial {
            settings.radial.centre = [320.0, 200.0];
        }
        if let Some((sweep, offset, drift)) = open_radial {
            settings.radial.closed_loop = false;
            settings.radial.sweep = sweep;
            settings.radial.spiral_offset = offset;
            settings.radial.centre_drift = drift;
            if offset != 0.0 {
                settings.count = 24;
                settings.radial.radius = if offset > 0.0 { 38.0 } else { 140.0 };
            } else if drift != [0.0; 2] {
                settings.count = 36;
                settings.radial.radius = 65.0;
            }
        } else {
            settings.stops =
                vec![PropertyStop::new(0, 0.0, 0.75), PropertyStop::new(2, 0.4, 1.0), PropertyStop::new(1, 1.0, 0.03)];
        }
        if transform_sample {
            settings.count = if mode == Mode::Radial { 17 } else { 12 };
            settings.linear.end[0] += 140.0;
            for (stop, (scale, rotation, alpha)) in settings.stops.iter_mut().zip([
                ([0.65, 1.15], 20.0, 0.75), ([1.05, 0.7], 320.0, 1.0), ([1.55, 1.0], 720.0, 0.45),
            ]) {
                stop.value.scale = scale;
                stop.value.rotation = rotation;
                stop.value.alpha = alpha;
            }
            if mode == Mode::Radial {
                settings.radial.centre = [270.0, 185.0];
                settings.radial.closed_loop = false;
                settings.radial.sweep = 630.0;
                settings.radial.radius = 58.0;
                settings.radial.spiral_offset = 26.0;
                settings.radial.centre_drift = [40.0, 12.0];
                settings.radial.rotate = true;
            }
        }
        match view {
            "array-easing-global" => settings.easing.preset = Easing::BackOut,
            _ if segment_sample => {
                settings.easing.mode = ArrayEaseMode::PerSegment;
                settings.easing.colour_effect = false;
                for (stop, preset) in settings.stops.iter_mut().zip([Easing::QuadOut, Easing::SineIn]) {
                    stop.value.outbound = Some(preset);
                }
                let selected = match view {
                    "array-easing-segment-middle" => 1,
                    "array-easing-segment-end" => 2,
                    _ => 0,
                };
                app.array_panel.selected_stop = settings.stops.get(selected).map(PropertyStop::marker_id);
            }
            _ => {}
        }
    }
    app.array_panel.refresh(&mut app.s);
}

pub(crate) fn text_had_escape(ctx: &egui::Context) -> bool {
    current_or_previous_text(ctx).is_some()
}

fn current_or_previous_text(ctx: &egui::Context) -> Option<Id> {
    // egui surrenders ordinary TextEdit focus before the frame on Escape. DragValue still
    // needs to see the key to discard the typed value rather than applying it on blur.
    let key = text_focus_id(ctx);
    let prior = ctx.data(|d| d.get_temp::<Option<Id>>(key)).flatten();
    focused_text(ctx).or_else(|| prior.filter(|id| ctx.memory(|m| m.had_focus_last_frame(*id))))
}

pub(crate) fn escape(app: &mut App, ctx: &egui::Context) {
    if !(app.s.array_preview_active() || app.array_panel.reset_pending)
        || app.modal_open()
        || app.palette.is_some()
        || app.dock_view.drag.is_some()
        || egui::Popup::is_any_open(ctx)
    {
        return;
    }
    if let Some(id) = current_or_previous_text(ctx) {
        if ctx.input(|input| input.key_pressed(Key::Escape)) {
            app.array_panel.cancel_text_edit(&mut app.s, EditGesture::Text(ctx.viewport_id(), id));
        }
        return;
    }
    if ctx.input_mut(|input| input.consume_key(Modifiers::NONE, Key::Escape)) {
        if app.array_panel.reset_pending {
            app.array_panel.reset_pending = false;
        } else {
            app.array_panel.finish(&mut app.s, false);
        }
    }
}

/// Called wherever the Array dock tab is drawn.
pub fn panel(app: &mut App, ui: &mut Ui) {
    app.array_panel.check_draft(&app.s);
    let eligible = app.s.array_preview_active() || source_origin(&app.s).is_some();
    let busy = app.playing
        || app.drag != Drag::None
        || app.text_edit.is_some()
        || app.pen.is_some()
        || app.modal_open()
        || app.file_dialog_open();
    let state = &mut app.array_panel;
    let prior = state.mode();
    let mut mode = prior;
    form_row(ui, FORM_LABEL_DOCK, tr!("Array type"), |ui| {
        egui::ComboBox::from_id_salt("array_type")
            .selected_text(match mode {
                None => tr!("None"),
                Some(Mode::Linear) => tr!("Linear"),
                Some(Mode::Radial) => tr!("Radial"),
            })
            .show_ui(ui, |ui| {
                ui.selectable_value(&mut mode, None, tr!("None"));
                ui.add_enabled_ui(eligible, |ui| {
                    ui.selectable_value(&mut mode, Some(Mode::Linear), tr!("Linear"));
                    ui.selectable_value(&mut mode, Some(Mode::Radial), tr!("Radial"));
                });
                ui.add_enabled(false, egui::Button::selectable(false, tr!("Grid")));
                ui.add_enabled(false, egui::Button::selectable(false, tr!("Along Path")));
            })
            .response
            .named(tr!("Array type"));
    });
    if mode != prior {
        if prior.is_none() && busy {
            state.error = Some(tr!("Finish the current edit before starting an array.").to_string());
        } else {
            state.select_mode(&mut app.s, mode);
            if state.active.is_some() {
                // Preview controls have their own input, independent of the stage and timeline.
                app.path_pick = None;
            }
            ui.ctx().request_repaint_of(egui::ViewportId::ROOT);
        }
    }
    if !eligible {
        ui.label(text(Type::Caption, tr!("Select one symbol instance on the active layer to make an array.")));
    }

    if let Some(mode) = state.mode() {
        ui.horizontal_wrapped(|ui| finish_buttons(state, &mut app.s, ui));
        if state.mode().is_some() && view_options(ui, state) {
            app.stage_cache.shown_state = None;
        }
        if state.mode().is_some() {
            let ctx = ui.ctx();
            let viewport = ctx.viewport_id();
            let gesture = ctx.dragged_id().or_else(|| ctx.drag_stopped_id()).map(|id| EditGesture::Pointer(viewport, id))
                .or_else(|| focused_text(ctx).map(|id| EditGesture::Text(viewport, id)));
            let before = state.before_edit(gesture);
            let changed = state.settings.as_mut().is_some_and(|settings| {
                ui.add_enabled_ui(!state.reset_pending, |ui| {
                    settings_ui(ui, settings, mode, state.stop_editor_epoch, &mut state.selected_stop, &mut app.scale_lock)
                }).inner
            });
            if changed {
                state.record_edit(before, gesture);
                state.refresh(&mut app.s);
                ui.ctx().request_repaint_of(egui::ViewportId::ROOT);
            }
        }
    }
    if let Some(error) = &state.error {
        ui.label(text(Type::Caption, crate::i18n::t_dyn(error)));
    }

    ui.add_space(8.0);
    if state.reset_pending {
        ui.label(text(Type::Body, tr!("Reset the remembered Array settings?")));
        ui.horizontal_wrapped(|ui| {
            if ui.button(tr!("Reset Settings")).clicked() {
                state.reset(&mut app.s);
                ui.ctx().request_repaint_of(egui::ViewportId::ROOT);
            }
            if ui.button(tr!("Keep Settings")).clicked() {
                state.reset_pending = false;
            }
        });
    } else if ui.add_enabled(state.settings.is_some(), egui::Button::new(tr!("Reset Settings to Defaults"))).clicked() {
        if state.customized() {
            state.reset_pending = true;
        } else {
            state.reset(&mut app.s);
            ui.ctx().request_repaint_of(egui::ViewportId::ROOT);
        }
    }
}

fn view_options(ui: &mut Ui, state: &mut State) -> bool {
    ui.horizontal_wrapped(|ui| {
        let dim = crate::theme::checkbox(ui, &mut state.dim_stage, tr!("Dim Stage")).changed();
        let tip = if state.show_gizmo {
            tr!("Hide Array Handles")
        } else {
            tr!("Show Array Handles")
        };
        let target = crate::theme::TARGET;
        let (rect, toggle) = ui.allocate_exact_size(egui::vec2(target + 4.0, target), egui::Sense::click());
        let hot = toggle.hovered() || toggle.has_focus();
        let (fill, icon) = if state.show_gizmo { (T.active, T.on_active) }
            else if hot { (T.hover, T.icon_hot) } else { (T.header, T.icon) };
        ui.painter().rect(rect, T.rc(), fill, egui::Stroke::new(1.0, T.border), egui::StrokeKind::Inside);
        crate::icons::paint(ui.painter(), egui::Rect::from_center_size(rect.center(), egui::Vec2::splat(crate::theme::TOOL_ICON)),
            crate::icons::Icon::ArrayHandles, icon);
        crate::a11y::control(ui, &toggle, egui::WidgetType::Button, tip, Some(state.show_gizmo));
        crate::a11y::focus_ring(ui, &toggle);
        let toggle = toggle.on_hover_text(tip);
        if toggle.clicked() {
            state.show_gizmo = !state.show_gizmo;
            state.gizmo_drag = None;
        }
        if dim || toggle.clicked() {
            ui.ctx().request_repaint_of(egui::ViewportId::ROOT);
        }
        dim
    }).inner
}

fn settings_ui(ui: &mut Ui, settings: &mut Settings, mode: Mode, stop_editor_epoch: u64, selected_stop: &mut Option<usize>, scale_lock: &mut bool) -> bool {
    let easing_open = settings.easing != ArrayEasing::default();
    let mut changed = crate::theme::section_with_default(ui, "array_easing", Section::Rule, tr!("Easing"), easing_open, |ui| {
        let index = settings.stops.iter().position(|stop| Some(stop.marker_id()) == *selected_stop).unwrap_or(0);
        let has_successor = index + 1 < settings.stops.len();
        easing_fields(ui, &mut settings.easing, settings.stops.get_mut(index), has_successor)
    }).unwrap_or(false);
    ui.add_space(4.0);
    section(ui, "array_stops", Section::Rule, tr!("Property stops"), None, |ui| {
        let id = ui.make_persistent_id(("array_stop_bar", stop_editor_epoch));
        let before = *selected_stop;
        changed |= crate::colour_panel::array_stop_bar(ui, id, &mut settings.stops, selected_stop, |ui, stop| {
            stop_properties(ui, stop, scale_lock)
        });
        if *selected_stop != before { ui.ctx().request_repaint(); }
    });
    ui.add_space(4.0);
    section(ui, "array_distribution", Section::Rule, tr!("Distribution"), None, |ui| {
        ui.horizontal_wrapped(|ui| {
            ui.label(text(Type::Label, tr!("Instances")));
            changed |= with_type(ui, Type::Data, |ui| ui.add(egui::DragValue::new(&mut settings.count).range(1..=500)))
                .named(tr!("Instances"))
                .changed();
        });
        match mode {
            Mode::Linear => {
                changed |= coordinates(ui, tr!("Start"), [tr!("Start X"), tr!("Start Y")], &mut settings.linear.start, " px");
                changed |= coordinates(ui, tr!("End"), [tr!("End X"), tr!("End Y")], &mut settings.linear.end, " px");
                changed |= linear_dimensions(ui, &mut settings.linear, settings.count);
            }
            Mode::Radial => changed |= radial_fields(ui, &mut settings.radial),
        }
        changed |= crate::theme::checkbox(ui, &mut settings.backwards, tr!("Stack first copy in front")).changed();
    });
    changed
}

fn easing_fields(ui: &mut Ui, easing: &mut ArrayEasing, selected: Option<&mut PropertyStop>, has_successor: bool) -> bool {
    fn mode_label(mode: ArrayEaseMode) -> &'static str {
        match mode {
            ArrayEaseMode::Global => tr!("Global"),
            ArrayEaseMode::PerSegment => tr!("Per Segment"),
        }
    }
    let before = easing.clone();
    let mut local_changed = false;
    form_row(ui, FORM_LABEL_DOCK, tr!("Mode"), |ui| {
        egui::ComboBox::from_id_salt("array_ease_mode")
            .width(144.0).truncate().selected_text(mode_label(easing.mode))
            .show_ui(ui, |ui| {
                for mode in [ArrayEaseMode::Global, ArrayEaseMode::PerSegment] {
                    ui.selectable_value(&mut easing.mode, mode, mode_label(mode));
                }
            }).response.named(tr!("Easing mode"));
    });
    form_row(ui, FORM_LABEL_DOCK, tr!("Ease"), |ui| {
        match easing.mode {
            ArrayEaseMode::Global => {
                if let Some(preset) = crate::ease_editor::named_picker(ui, "array_ease_preset", &easing.preset, 144.0, tr!("Ease")) {
                    easing.preset = preset;
                }
            }
            ArrayEaseMode::PerSegment => {
                let marker = selected.as_ref().map(|stop| stop.marker_id());
                let current = selected.as_ref().and_then(|stop| stop.value.outbound.as_ref()).unwrap_or(&Easing::Linear);
                let pick = ui.add_enabled_ui(has_successor, |ui| {
                    ui.push_id(marker, |ui| crate::ease_editor::named_picker(ui, "array_segment_preset", current, 144.0, tr!("Ease"))).inner
                });
                pick.response.on_disabled_hover_text(tr!("The final stop has no outgoing segment."));
                if let Some(preset) = pick.inner
                    && let Some(stop) = selected
                {
                    let next = (preset != Easing::Linear).then_some(preset);
                    local_changed = stop.value.outbound != next;
                    stop.value.outbound = next;
                }
            }
        }
    });
    ui.label(text(Type::Label, tr!("Apply to")));
    let labels = [tr!("Distribution"), tr!("Transform"), tr!("Colour Effect"), tr!("Filters")];
    let font = egui::TextStyle::Body.resolve(ui.style());
    let longest = labels.iter().map(|label| {
        ui.painter().layout_no_wrap((*label).into(), font.clone(), ui.visuals().text_color()).size().x
    }).fold(0.0, f32::max);
    let gap = ui.spacing().item_spacing.x;
    let columns = if 2.0 * (longest + crate::theme::TARGET) + gap <= ui.available_width() { 2 } else { 1 };
    let width = (ui.available_width() - (columns - 1) as f32 * gap) / columns as f32;
    let mut filters = false;
    egui::Grid::new("array_ease_targets").num_columns(columns).min_col_width(width).max_col_width(width)
        .show(ui, |ui| {
            let targets = [
                (&mut easing.distribution, true), (&mut easing.transform, true),
                (&mut easing.colour_effect, true), (&mut filters, false),
            ];
            for (index, (label, (target, enabled))) in labels.into_iter().zip(targets).enumerate() {
                if enabled {
                    crate::theme::checkbox(ui, target, label);
                } else {
                    ui.add_enabled_ui(false, |ui| crate::theme::checkbox(ui, target, label))
                        .inner.on_disabled_hover_text(tr!("Available when filter stops support interpolation."));
                }
                if (index + 1).is_multiple_of(columns) {
                    ui.end_row();
                }
            }
        });
    local_changed || *easing != before
}

fn radial_fields(ui: &mut Ui, radial: &mut Radial) -> bool {
    let mut changed = coordinates(ui, tr!("Centre"), [tr!("Centre X"), tr!("Centre Y")], &mut radial.centre, " px");
    let range = -RADIAL_FIELD_LIMIT..=RADIAL_FIELD_LIMIT;
    changed |= field(ui, tr!("Radius"), &mut radial.radius, " px", 0.0..=RADIAL_FIELD_LIMIT);
    changed |= field(ui, tr!("Start angle"), &mut radial.start, " °", range.clone());
    let loop_changed = crate::theme::checkbox(ui, &mut radial.closed_loop, tr!("Closed Loop")).changed();
    let limit = radial.sweep_limit().min(RADIAL_FIELD_LIMIT);
    if loop_changed && radial.closed_loop {
        radial.sweep = radial.sweep.clamp(-limit, limit);
    }
    changed |= loop_changed;
    changed |= field(ui, tr!("Sweep"), &mut radial.sweep, " °", -limit..=limit);
    if !radial.closed_loop {
        changed |= field(ui, tr!("Spiral Offset"), &mut radial.spiral_offset, tr!(" px/turn"), range);
        changed |= coordinates(ui, tr!("Centre Drift"), [tr!("Centre Drift X"), tr!("Centre Drift Y")],
            &mut radial.centre_drift, tr!(" px/turn"));
    }
    changed |= crate::theme::checkbox(ui, &mut radial.rotate, tr!("Rotate instances")).changed();
    changed
}

fn stop_properties(ui: &mut Ui, stop: &mut PropertyStop, scale_lock: &mut bool) -> bool {
    egui::Frame::new()
        .fill(T.panel_alt)
        .stroke(egui::Stroke::new(1.0, T.border))
        .corner_radius(T.radius_control)
        .inner_margin(egui::Margin::symmetric(8, 4))
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            let marker = stop.marker_id();
            let transform = section(ui, "array_stop_transform", Section::Rule, tr!("Transform"), None, |ui| {
                ui.push_id(("transform", marker), |ui| transform_stop(ui, &mut stop.value, scale_lock)).inner
            }).unwrap_or(false);
            let colour = section(ui, "array_stop_colour_effect", Section::Rule, tr!("Colour Effect"), None, |ui| {
                ui.push_id(("colour", marker), |ui| ui.horizontal_wrapped(|ui| {
                    ui.label(text(Type::Label, tr!("Alpha")));
                    let mut value = stop.value.alpha * 100.0;
                    let changed = with_type(ui, Type::Data, |ui| {
                        ui.add(egui::DragValue::new(&mut value).range(0.0..=100.0).suffix(" %").max_decimals(1))
                    }).named(tr!("Alpha")).changed();
                    if changed {
                        stop.value.alpha = value / 100.0;
                    }
                    changed
                }).inner).inner
            }).unwrap_or(false);
            transform || colour
        }).inner
}

fn transform_stop(ui: &mut Ui, stop: &mut ArrayPropertyStop, scale_lock: &mut bool) -> bool {
    let mut changed = with_type(ui, Type::Data, |ui| ui.horizontal_wrapped(|ui| {
        ui.label(text(Type::Label, tr!("Scale")));
        let [mut x, mut y] = stop.scale.map(|value| value * 100.0);
        let old = (x, y);
        let limit = 100.0 * ArrayPropertyStop::SCALE_LIMIT;
        let value = |ui: &mut Ui, value: &mut f32, axis: &str, name: &str| {
            ui.add(egui::DragValue::new(value).range(-limit..=limit).clamp_existing_to_range(false)
                .prefix(axis).suffix(" %").speed(0.5).max_decimals(1)).named(name).changed()
        };
        let x_changed = value(ui, &mut x, "X ", tr!("Scale X"));
        let y_changed = value(ui, &mut y, "Y ", tr!("Scale Y"));
        let icon = if *scale_lock { crate::icons::Icon::Lock } else { crate::icons::Icon::Unlock };
        if crate::timeline::icon_button_wide(ui, tr!("Lock scale"), Some(*scale_lock), icon, 20.0).clicked() {
            *scale_lock = !*scale_lock;
        }
        if x_changed || y_changed {
            if *scale_lock { (x, y) = crate::panels::linked_scale(old, (x, y), x_changed); }
            stop.scale = [x / 100.0, y / 100.0];
        }
        x_changed || y_changed
    }).inner);
    let limit = ArrayPropertyStop::ROTATION_LIMIT;
    changed |= dimension(ui, tr!("Rotation"), &mut stop.rotation, -limit..=limit, "°", true);
    changed
}

fn linear_dimensions(ui: &mut Ui, endpoints: &mut Endpoints, count: u32) -> bool {
    let [dx, dy] = endpoints.extent();
    let mut length = dx.hypot(dy);
    let mut direction = if length > 0.0 { dy.atan2(dx).to_degrees() } else { 0.0 };
    let mut changed = dimension(ui, tr!("Direction"), &mut direction, -180.0..=180.0, "°", length > 0.0);
    changed |= dimension(ui, tr!("Length"), &mut length, 0.0..=1_000_000.0, " px", true);
    if changed {
        let (sin, cos) = direction.to_radians().sin_cos();
        endpoints.end = [endpoints.start[0] + length * cos, endpoints.start[1] + length * sin];
    }
    let spacing = endpoints.spacing(count);
    for (axis, label) in [tr!("Spacing X"), tr!("Spacing Y")].into_iter().enumerate() {
        let mut distance = spacing.map_or(0.0, |v| v[axis]);
        if dimension(ui, label, &mut distance, -1_000_000.0..=1_000_000.0, " px", spacing.is_some()) {
            endpoints.end[axis] = endpoints.start[axis] + distance * count.saturating_sub(1) as f32;
            changed = true;
        }
    }
    changed
}

fn dimension(ui: &mut Ui, label: &str, value: &mut f32, range: std::ops::RangeInclusive<f32>, suffix: &str, enabled: bool) -> bool {
    form_row(ui, FORM_LABEL_DOCK, label, |ui| with_type(ui, Type::Data, |ui| {
        let field = egui::DragValue::new(value).range(range).clamp_existing_to_range(false).suffix(suffix).max_decimals(1);
        let field = if enabled { field } else { field.custom_formatter(|_, _| "—".to_owned()).suffix("") };
        ui.add_enabled(enabled, field).named(label).changed()
    }))
}

fn coordinates(ui: &mut Ui, label: &str, [x_name, y_name]: [&str; 2], [x, y]: &mut [f32; 2], suffix: &str) -> bool {
    form_row(ui, FORM_LABEL_DOCK, label, |ui| {
        let mut changed = false;
        for (value, axis, name) in [(x, "X ", x_name), (y, "Y ", y_name)] {
            changed |= with_type(ui, Type::Data, |ui| {
                ui.add(egui::DragValue::new(value).range(-100_000.0..=100_000.0).clamp_existing_to_range(false)
                    .prefix(axis).suffix(suffix).max_decimals(1))
            }).named(name).changed();
        }
        changed
    })
}

fn field(ui: &mut Ui, label: &str, value: &mut f32, suffix: &str, range: std::ops::RangeInclusive<f32>) -> bool {
    ui.horizontal_wrapped(|ui| {
        ui.label(text(Type::Label, label));
        with_type(ui, Type::Data, |ui| {
            ui.add(
                egui::DragValue::new(value)
                    .range(range)
                    .clamp_existing_to_range(false)
                    .suffix(suffix)
                    .speed(1.0)
                    .max_decimals(1),
            )
        })
        .named(label)
        .changed()
    })
    .inner
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::{Event, PointerButton, Pos2, Vec2, pos2, vec2};
    use egui::accesskit::{Action, ActionData, ActionRequest, Node, NodeId, Role, TreeId};

    fn instance(s: &mut Session, x: f32, name: &str) {
        s.execute("draw.rect", &json!({"x": x, "y": 50, "w": 24, "h": 14})).unwrap();
        s.execute("modify.convert_to_symbol", &json!({"name": name})).unwrap();
    }

    #[test]
    fn switches_replay_from_the_source_and_commit_once() {
        let mut s = Session::default();
        instance(&mut s, 50.0, "Source");
        let source = s.selection.first().unwrap();
        let (doc, selection, steps) = (s.doc.clone(), s.selection.clone(), s.history_len());
        let mut state = State::default();
        assert_eq!(state.mode(), None);
        state.select_mode(&mut s, Some(Mode::Linear));
        assert_eq!(s.selection.len(), 10);
        assert_eq!(s.selection.first(), Some(source));
        let settings = state.settings.as_mut().unwrap();
        settings.linear.end = [200.0, 170.0];
        settings.radial.radius = 60.0;
        settings.count = 7;
        settings.stops.insert(1, PropertyStop::new(2, 0.4, 0.25));
        state.refresh(&mut s);
        let linear = s.doc.clone();
        for _ in 0..3 {
            state.select_mode(&mut s, Some(Mode::Radial));
            assert!(s.doc != linear);
            assert_eq!(s.selection.len(), 7);
            assert_eq!(s.selection.first(), Some(source));
            state.select_mode(&mut s, Some(Mode::Linear));
            assert!(s.doc == linear, "replaying Linear keeps its spacing and stops");
        }
        assert_eq!(s.history_len(), steps);
        state.finish(&mut s, true);
        assert_eq!(state.mode(), None);
        assert_eq!(s.selection.len(), 7);
        assert_eq!(s.history_len(), steps + 1);
        assert_eq!(s.undo_label(), Some("Array"));
        assert!(state.settings.is_none());
        s.undo().unwrap();
        assert!(s.doc == doc && s.selection == selection);
    }

    #[test]
    fn none_preserves_the_draft_for_the_same_source_and_starts_fresh_on_a_new_source() {
        let mut s = Session::default();
        instance(&mut s, 50.0, "First");
        let first = s.selection.first().unwrap();
        let doc = s.doc.clone();
        let steps = s.history_len();
        let mut state = State::default();
        state.select_mode(&mut s, Some(Mode::Linear));
        state.settings.as_mut().unwrap().count = 3;
        state.refresh(&mut s);
        let remembered = state.settings.clone();
        state.select_mode(&mut s, None);
        assert!(s.doc == doc && s.selection.first() == Some(first) && !s.in_gesture());
        assert_eq!(s.history_len(), steps);
        assert_eq!(state.settings, remembered);
        state.select_mode(&mut s, Some(Mode::Linear));
        assert_eq!(s.selection.len(), 3);
        assert_eq!(state.settings, remembered);
        state.select_mode(&mut s, None);

        instance(&mut s, 400.0, "Second");
        let second = s.selection.first().unwrap();
        let origin = source_origin(&s).unwrap();
        let next_doc = s.doc.clone();
        state.select_mode(&mut s, Some(Mode::Radial));
        assert_eq!(s.selection.first(), Some(second));
        assert_eq!(s.selection.len(), 10);
        assert_eq!(state.settings.as_ref(), Some(&Settings::at(origin)));
        state.settings.as_mut().unwrap().radial.spiral_offset = 30.0;
        assert!(state.customized());
        state.reset(&mut s);
        assert!(!state.customized());
        assert!(s.array_preview_active() && s.selection.len() == 10);
        state.finish(&mut s, false);
        assert!(s.doc == next_doc && s.selection.first() == Some(second));
    }

    fn frame(ctx: &egui::Context, app: &mut App, events: Vec<Event>) -> Vec<(NodeId, Node)> {
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1600.0, 940.0))),
            events,
            ..Default::default()
        };
        let mut out = ctx.run_ui(input, |ui| app.frame(ui));
        out.textures_delta.clear();
        out.platform_output.accesskit_update.map(|u| u.nodes).unwrap_or_default()
    }

    fn nodes(ctx: &egui::Context, app: &mut App) -> Vec<(NodeId, Node)> {
        frame(ctx, app, vec![]);
        frame(ctx, app, vec![])
    }

    fn click(ctx: &egui::Context, app: &mut App, node: &Node) {
        let rect = node.bounds().unwrap();
        let pos = egui::pos2(((rect.x0 + rect.x1) / 2.0) as f32, ((rect.y0 + rect.y1) / 2.0) as f32);
        frame(ctx, app, vec![Event::PointerMoved(pos)]);
        for pressed in [true, false] {
            frame(ctx, app, vec![Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: Modifiers::NONE }]);
        }
    }

    fn key(key: Key, pressed: bool) -> Event {
        Event::Key { key, physical_key: None, pressed, repeat: false, modifiers: Modifiers::NONE }
    }

    fn linear_app() -> (App, egui::Context) {
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        let mut s = Session::default();
        instance(&mut s, 180.0, "Source");
        let mut app = App::with_context(&ctx, s, Some(std::path::PathBuf::from("unused.png")));
        app.show_panel(Panel::Array);
        for group in &mut app.dock.right.groups {
            group.folded = !group.tabs.contains(&Panel::Array);
        }
        (app, ctx)
    }

    fn screen(app: &App, [x, y]: [f32; 2]) -> Pos2 {
        let zoom = app.view.zoom;
        let size = vec2(app.s.doc.stage.width as f32, app.s.doc.stage.height as f32);
        let origin = app.stage_cache.canvas.center() + app.view.pan - size * zoom * 0.5;
        pos2(origin.x.round() + x * zoom, origin.y.round() + y * zoom)
    }

    fn pointer_button(pos: Pos2, pressed: bool) -> Event {
        Event::PointerButton { pos, button: PointerButton::Primary, pressed, modifiers: Modifiers::NONE }
    }

    #[test]
    #[ignore = "local Array drag timing on the CPU stage"]
    fn measure_linear_drag() {
        use std::time::Instant;
        fn median(mut samples: Vec<f64>) -> f64 {
            samples.sort_by(f64::total_cmp);
            samples[samples.len() / 2]
        }
        for count in [10, 200] {
            let (mut app, ctx) = linear_app();
            screenshot(&mut app, "array");
            app.prefs.render_mode = crate::prefs::RenderMode::Immediate;
            app.array_panel.settings.as_mut().unwrap().count = count;
            app.array_panel.refresh(&mut app.s);
            app.view.fit = false;
            app.view.zoom = 0.65;
            nodes(&ctx, &mut app);
            let initial = app.array_panel.gizmo_endpoints().unwrap();
            for (region, end_x) in [("stage", 480.0), ("pasteboard", 720.0)] {
                let mut ends = initial;
                ends.end[0] = end_x;
                app.array_panel.set_endpoints(&mut app.s, ends);
                nodes(&ctx, &mut app);
                let from = screen(&app, ends.end);
                assert!(app.stage_cache.canvas.contains(from));
                frame(&ctx, &mut app, vec![Event::PointerMoved(from)]);
                frame(&ctx, &mut app, vec![pointer_button(from, true)]);
                let mut wall = Vec::new();
                let mut render = Vec::new();
                let mut to = from;
                for i in 1..=12 {
                    to = from + vec2(i as f32 * 3.0, i as f32 * 0.75);
                    let timer = Instant::now();
                    frame(&ctx, &mut app, vec![Event::PointerMoved(to)]);
                    wall.push(timer.elapsed().as_secs_f64() * 1_000.0);
                    render.push(f64::from(app.stage_cache.ms));
                }
                near(app.array_panel.gizmo_endpoints().unwrap().end, [end_x + 36.0 / 0.65, ends.end[1] + 9.0 / 0.65]);
                frame(&ctx, &mut app, vec![pointer_button(to, false)]);
                let mut update = Vec::new();
                let mut selection = Vec::new();
                for i in 1..=12 {
                    ends.end[0] -= i as f32;
                    let timer = Instant::now();
                    app.array_panel.set_endpoints(&mut app.s, ends);
                    update.push(timer.elapsed().as_secs_f64() * 1_000.0);
                    let timer = Instant::now();
                    std::hint::black_box(app.s.selection_bounds());
                    selection.push(timer.elapsed().as_secs_f64() * 1_000.0);
                }
                eprintln!("Array {count:3} {region:10} median ms: frame {:.3}, render {:.3}, update {:.3}, selection bounds {:.3}",
                    median(wall), median(render), median(update), median(selection));
            }
        }
    }

    #[test]
    #[ignore = "local spiral drag timing on the CPU stage"]
    fn measure_radial_drag() {
        use std::time::Instant;
        for count in [10, 200] {
            let (mut app, ctx) = linear_app();
            screenshot(&mut app, "array-radial-open:720,18");
            app.prefs.render_mode = crate::prefs::RenderMode::Immediate;
            app.view.fit = false;
            app.view.zoom = 0.65;
            for (region, x) in [("stage", 330.0), ("pasteboard", 690.0)] {
                let settings = app.array_panel.settings.as_mut().unwrap();
                settings.count = count;
                settings.radial.centre[0] = x;
                settings.radial.sweep = 700.0;
                app.array_panel.refresh(&mut app.s);
                nodes(&ctx, &mut app);
                let end = app.array_panel.settings.as_ref().unwrap().radial.point(1.0);
                let from = screen(&app, [end.x, end.y]);
                assert!(app.stage_cache.canvas.contains(from));
                frame(&ctx, &mut app, vec![Event::PointerMoved(from)]);
                frame(&ctx, &mut app, vec![pointer_button(from, true)]);
                let mut wall = Vec::new();
                let mut to = from;
                for i in 1..=12 {
                    to = from + vec2(i as f32 * 2.5, i as f32 * 1.5);
                    let timer = Instant::now();
                    frame(&ctx, &mut app, vec![Event::PointerMoved(to)]);
                    wall.push(timer.elapsed().as_secs_f64() * 1_000.0);
                }
                assert!(app.array_panel.gizmo_drag.is_some());
                let moved = app.array_panel.settings.as_ref().unwrap().radial;
                assert_ne!(moved.sweep, 700.0);
                frame(&ctx, &mut app, vec![pointer_button(to, false)]);
                wall.sort_by(f64::total_cmp);
                eprintln!("Spiral {count:3} {region:10} median frame ms {:.3}", wall[wall.len() / 2]);
            }
        }
    }

    fn drag_gizmo(ctx: &egui::Context, app: &mut App, from: Pos2, by: Vec2, finish: bool) {
        frame(ctx, app, vec![Event::PointerMoved(from)]);
        frame(ctx, app, vec![pointer_button(from, true)]);
        frame(ctx, app, vec![Event::PointerMoved(from + by)]);
        if finish {
            frame(ctx, app, vec![pointer_button(from + by, false)]);
        }
    }

    fn set_number(ctx: &egui::Context, app: &mut App, target_node: NodeId, value: f64) {
        frame(ctx, app, vec![Event::AccessKitActionRequest(ActionRequest {
            action: Action::SetValue, target_tree: TreeId::ROOT, target_node, data: Some(ActionData::NumericValue(value)),
        })]);
    }

    fn history_key(ctx: &egui::Context, app: &mut App, key: Key, extra: Modifiers) {
        let modifiers = Modifiers::COMMAND | extra;
        frame(ctx, app, vec![Event::Key { key, physical_key: None, pressed: true, repeat: false, modifiers }]);
        frame(ctx, app, vec![Event::Key { key, physical_key: None, pressed: false, repeat: false, modifiers }]);
    }

    #[test]
    fn array_history_coalesces_stage_drags_and_commits_one_document_step() {
        use anicraft_engine::geom::Vec2 as Point;
        let (mut app, ctx) = linear_app();
        let (base, source, steps) = (app.s.doc.clone(), app.s.selection.clone(), app.s.history_len());
        app.array_panel.select_mode(&mut app.s, Some(Mode::Linear));
        nodes(&ctx, &mut app);
        let initial = app.array_panel.settings.clone();
        history_key(&ctx, &mut app, Key::Z, Modifiers::NONE);
        assert_eq!(app.array_panel.settings, initial);
        assert!(app.s.array_preview_active() && app.s.committed_document().0 == &base);

        let from = screen(&app, initial.as_ref().unwrap().linear.end);
        drag_gizmo(&ctx, &mut app, from, vec2(12.0, 8.0), false);
        frame(&ctx, &mut app, vec![Event::PointerMoved(from + vec2(30.0, 20.0))]);
        frame(&ctx, &mut app, vec![Event::PointerMoved(from + vec2(44.0, 26.0))]);
        frame(&ctx, &mut app, vec![pointer_button(from + vec2(44.0, 26.0), false)]);
        let linear = app.array_panel.settings.clone();
        assert_ne!(linear, initial);
        history_key(&ctx, &mut app, Key::Z, Modifiers::NONE);
        assert_eq!(app.array_panel.settings, initial);
        assert!(!app.array_panel.can_step(false));
        history_key(&ctx, &mut app, Key::Z, Modifiers::SHIFT);
        assert_eq!(app.array_panel.settings, linear);

        app.array_panel.select_mode(&mut app.s, Some(Mode::Radial));
        assert!(app.perform("edit.undo", json!({})).is_some());
        assert_eq!(app.array_panel.mode(), Some(Mode::Linear));
        assert_eq!(app.array_panel.settings, linear);
        history_key(&ctx, &mut app, Key::Y, Modifiers::NONE);
        assert_eq!(app.array_panel.mode(), Some(Mode::Radial));
        let radial = Radial { centre: [260.0, 220.0], radius: 80.0, start: 0.0, sweep: 90.0,
            closed_loop: false, spiral_offset: 0.0, centre_drift: [70.0, -65.0], rotate: true };
        app.array_panel.set_gizmo_geometry(&mut app.s, Geometry::Radial(radial), None);
        nodes(&ctx, &mut app);
        let before = app.array_panel.settings.clone();
        let preview = app.s.doc.clone();
        let project = |app: &App, p: Point| screen(app, [p.x, p.y]);
        let from = project(&app, radial.point(1.0));
        let to = project(&app, Radial { sweep: 130.0, spiral_offset: 60.0, ..radial }.point(1.0));
        drag_gizmo(&ctx, &mut app, from, (to - from) * 0.5, false);
        frame(&ctx, &mut app, vec![Event::PointerMoved(to)]);
        frame(&ctx, &mut app, vec![pointer_button(to, false)]);
        let after = app.array_panel.settings.clone();
        let ended = after.as_ref().unwrap().radial;
        assert!((ended.sweep - 130.0).abs() < 0.01 && (ended.spiral_offset - 60.0).abs() < 0.05);
        history_key(&ctx, &mut app, Key::Z, Modifiers::NONE);
        assert_eq!(app.array_panel.settings, before);
        assert!(app.s.doc == preview && app.s.committed_document().0 == &base);
        history_key(&ctx, &mut app, Key::Y, Modifiers::NONE);
        assert_eq!(app.array_panel.settings, after);
        let final_preview = app.s.doc.clone();
        let shown = nodes(&ctx, &mut app);
        click(&ctx, &mut app, &shown.iter().find(|(_, n)| n.label() == Some("Commit")).unwrap().1);
        assert_eq!(app.s.history_len(), steps + 1);
        assert!(app.array_panel.settings.is_none() && !app.s.array_preview_active());
        history_key(&ctx, &mut app, Key::Z, Modifiers::NONE);
        assert!(app.s.doc == base && app.s.selection == source);
        history_key(&ctx, &mut app, Key::Y, Modifiers::NONE);
        assert!(app.s.doc == final_preview);
    }

    #[test]
    fn array_history_coalesces_numeric_drags_and_typing_and_preserves_text_escape() {
        let (mut app, ctx) = linear_app();
        app.array_panel.select_mode(&mut app.s, Some(Mode::Linear));
        let shown = |app: &mut App, label: &str| nodes(&ctx, app).into_iter().find(|(_, n)| n.label() == Some(label)).unwrap();
        let initial = app.array_panel.settings.clone();
        let bounds = shown(&mut app, "Scale X").1.bounds().unwrap();
        let from = pos2(((bounds.x0 + bounds.x1) / 2.0) as f32, ((bounds.y0 + bounds.y1) / 2.0) as f32);
        drag_gizmo(&ctx, &mut app, from, vec2(20.0, 0.0), false);
        frame(&ctx, &mut app, vec![Event::PointerMoved(from + vec2(46.0, 0.0))]);
        frame(&ctx, &mut app, vec![pointer_button(from + vec2(46.0, 0.0), false)]);
        let scaled = app.array_panel.settings.clone();
        assert_ne!(initial, scaled);
        assert_eq!(app.array_panel.active.as_ref().unwrap().history.undo.len(), 1);

        let edit = |app: &mut App| {
            let field = shown(app, "Rotation").1;
            click(&ctx, app, &field);
            frame(&ctx, app, vec![]);
        };
        let text = |app: &mut App, value: &str| frame(&ctx, app, vec![Event::Text(value.to_owned())]);
        edit(&mut app);
        text(&mut app, "7");
        text(&mut app, "2");
        assert_eq!(app.array_panel.active.as_ref().unwrap().history.undo.len(), 1);
        history_key(&ctx, &mut app, Key::Z, Modifiers::NONE);
        assert!(ctx.text_edit_focused());
        assert_eq!(app.array_panel.active.as_ref().unwrap().history.undo.len(), 1);
        frame(&ctx, &mut app, vec![Event::Key { key: Key::A, physical_key: None, pressed: true, repeat: false,
            modifiers: Modifiers::COMMAND }, Event::Text("720".to_owned())]);
        frame(&ctx, &mut app, vec![key(Key::Enter, true)]);
        frame(&ctx, &mut app, vec![key(Key::Enter, false)]);
        assert_eq!(app.array_panel.settings.as_ref().unwrap().stops[0].value.rotation, 720.0);
        assert_eq!(app.array_panel.active.as_ref().unwrap().history.undo.len(), 2);
        history_key(&ctx, &mut app, Key::Z, Modifiers::NONE);
        assert_eq!(app.array_panel.settings, scaled);

        edit(&mut app);
        text(&mut app, "97");
        frame(&ctx, &mut app, vec![key(Key::Escape, true)]);
        frame(&ctx, &mut app, vec![key(Key::Escape, false)]);
        assert_eq!(app.array_panel.settings, scaled);
        assert!(app.array_panel.can_step(true) && app.s.array_preview_active());
        let alpha = shown(&mut app, "Alpha").0;
        set_number(&ctx, &mut app, alpha, 45.0);
        assert_eq!(app.array_panel.settings.as_ref().unwrap().stops[0].value.alpha, 0.45);
        assert!(!app.array_panel.can_step(true));
        history_key(&ctx, &mut app, Key::Z, Modifiers::NONE);
        assert_eq!(app.array_panel.settings, scaled);
        history_key(&ctx, &mut app, Key::Z, Modifiers::NONE);
        assert_eq!(app.array_panel.settings, initial);
    }

    #[test]
    fn array_history_restores_stop_creation_movement_properties_and_removal() {
        let (mut app, ctx) = linear_app();
        app.array_panel.select_mode(&mut app.s, Some(Mode::Linear));
        let node = |app: &mut App, label| nodes(&ctx, app).into_iter().find(|(_, n)| n.label() == Some(label)).unwrap();
        let settings = |app: &App| app.array_panel.settings.clone().unwrap();
        let initial = settings(&app);
        for label in ["Dim Stage", "Hide Array Handles"] {
            let control = node(&mut app, label).1;
            click(&ctx, &mut app, &control);
        }
        assert!(!app.array_panel.can_step(false));
        let track = node(&mut app, "Array: add a property stop").1;
        click(&ctx, &mut app, &track);
        let added = settings(&app);
        assert_eq!(added.stops.len(), 3);
        let added_marker = Some(added.stops[1].marker_id());
        assert_eq!(app.array_panel.selected_stop, added_marker);
        let stop = nodes(&ctx, &mut app).into_iter().find(|(_, n)| n.label().is_some_and(|name| name.starts_with("Array stop 2 of"))).unwrap().1;
        let rect = stop.bounds().unwrap();
        let from = pos2(((rect.x0 + rect.x1) / 2.0) as f32, ((rect.y0 + rect.y1) / 2.0) as f32);
        drag_gizmo(&ctx, &mut app, from, vec2(20.0, 0.0), false);
        frame(&ctx, &mut app, vec![Event::PointerMoved(from + vec2(40.0, 0.0))]);
        frame(&ctx, &mut app, vec![pointer_button(from + vec2(40.0, 0.0), false)]);
        let moved = settings(&app);
        assert_ne!(moved, added);
        let alpha = node(&mut app, "Alpha").0;
        set_number(&ctx, &mut app, alpha, 30.0);
        let coloured = settings(&app);
        let scale_y = node(&mut app, "Scale Y").0;
        set_number(&ctx, &mut app, scale_y, 145.0);
        let edited = settings(&app);
        assert_eq!(edited.stops[1].value.alpha, 0.30);
        assert_eq!(edited.stops[1].value.scale[1], 1.45);
        let remove = node(&mut app, "Remove array stop").1;
        click(&ctx, &mut app, &remove);
        assert_eq!(settings(&app).stops.len(), 2);

        for target in [edited.clone(), coloured, moved, added, initial] {
            history_key(&ctx, &mut app, Key::Z, Modifiers::NONE);
            assert_eq!(settings(&app), target);
            if target.stops.len() == 3 { assert_eq!(app.array_panel.selected_stop, added_marker); }
        }
        assert!(!app.array_panel.can_step(false));
        for _ in 0..4 { history_key(&ctx, &mut app, Key::Y, Modifiers::NONE); }
        assert_eq!(settings(&app), edited);
        assert!(!app.array_panel.dim_stage && !app.array_panel.show_gizmo);
        let copies = app.s.selected_elements();
        assert!(copies.iter().any(|(_, element)| element.color.alpha() < 0.8));
        assert!(copies.iter().any(|(_, element)| element.transform.m[3] > 1.2));
    }

    #[test]
    fn easing_controls_preview_and_follow_session_history_and_draft_lifecycle() {
        let (mut app, ctx) = linear_app();
        app.array_panel.select_mode(&mut app.s, Some(Mode::Linear));
        let original = app.array_panel.settings.clone().unwrap();
        let steps = app.s.history_len();
        let node = |app: &mut App, label: &str| {
            nodes(&ctx, app).into_iter().find(|(_, node)| node.label() == Some(label))
                .unwrap_or_else(|| panic!("missing Array control {label}")).1
        };
        let choose = |app: &mut App, control, option| {
            let combo = nodes(&ctx, app).into_iter().find(|(_, node)| {
                node.role() == Role::ComboBox && node.label() == Some(control)
            }).unwrap().1;
            click(&ctx, app, &combo);
            let option = node(app, option);
            click(&ctx, app, &option);
            nodes(&ctx, app);
        };
        let sampled_x = |app: &App| app.s.selected_elements()[3].1.transform.m[4];
        let line_x = sampled_x(&app);
        let header = node(&mut app, "Easing");
        click(&ctx, &mut app, &header);
        assert!(node(&mut app, "Filters").is_disabled());
        choose(&mut app, "Ease", "Sine Out");
        assert_eq!(app.array_panel.settings.as_ref().unwrap().easing.preset, Easing::SineOut);
        assert!(sampled_x(&app) > line_x);
        history_key(&ctx, &mut app, Key::Z, Modifiers::NONE);
        assert_eq!(sampled_x(&app), line_x);
        choose(&mut app, "Ease", "Back Out");
        assert!(sampled_x(&app) > line_x);
        choose(&mut app, "Easing mode", "Per Segment");
        assert_eq!(sampled_x(&app), line_x);
        choose(&mut app, "Ease", "Quad Out");
        let target = nodes(&ctx, &mut app).into_iter().find(|(_, n)| {
            n.role() == Role::CheckBox && n.label() == Some("Distribution")
        }).unwrap().1;
        click(&ctx, &mut app, &target);
        assert_eq!(sampled_x(&app), line_x);
        let draft = app.array_panel.settings.clone().unwrap();
        assert_eq!(draft.stops[0].value.outbound, Some(Easing::QuadOut));
        assert_eq!(draft.stops[1], original.stops[1]);
        assert_eq!(draft.easing, ArrayEasing {
            mode: ArrayEaseMode::PerSegment, preset: Easing::BackOut, distribution: false, ..ArrayEasing::default()
        });
        assert_eq!(app.array_panel.active.as_ref().unwrap().history.undo.len(), 4);
        history_key(&ctx, &mut app, Key::Z, Modifiers::NONE);
        assert!(sampled_x(&app) > line_x);
        history_key(&ctx, &mut app, Key::Y, Modifiers::NONE);
        assert_eq!(app.array_panel.settings.as_ref(), Some(&draft));
        assert_eq!(sampled_x(&app), line_x);

        app.array_panel.select_mode(&mut app.s, Some(Mode::Radial));
        assert_eq!(app.array_panel.settings.as_ref(), Some(&draft));
        app.array_panel.select_mode(&mut app.s, None);
        assert_eq!(app.s.history_len(), steps);
        app.array_panel.select_mode(&mut app.s, Some(Mode::Linear));
        assert_eq!(app.array_panel.settings.as_ref(), Some(&draft));
        assert!(!app.array_panel.can_step(false));
        app.array_panel.finish(&mut app.s, true);
        assert_eq!(app.s.history_len(), steps + 1);
        let baked = app.s.selection.ids().next().unwrap();
        app.s.select_only([baked]);
        app.array_panel.select_mode(&mut app.s, Some(Mode::Linear));
        assert_eq!(app.array_panel.settings.as_ref().unwrap().easing, ArrayEasing::default());
        app.array_panel.finish(&mut app.s, false);
    }

    #[test]
    fn outbound_picker_follows_the_selected_stop_and_history() {
        let (mut app, ctx) = linear_app();
        app.array_panel.select_mode(&mut app.s, Some(Mode::Linear));
        assert_eq!(app.array_panel.selected_stop, Some(0));
        let named = |app: &mut App, label: &str| {
            nodes(&ctx, app).into_iter().find(|(_, n)| n.label() == Some(label))
                .unwrap_or_else(|| panic!("missing {label}")).1
        };
        let picker = |app: &mut App, label: &str| {
            nodes(&ctx, app).into_iter().find(|(_, n)| n.role() == Role::ComboBox && n.label() == Some(label)).unwrap().1
        };
        let choose = |app: &mut App, control, option| {
            let combo = picker(app, control);
            click(&ctx, app, &combo);
            let option = named(app, option);
            click(&ctx, app, &option);
        };
        let select = |app: &mut App, number| {
            let prefix = format!("Array stop {number} of");
            let stop = nodes(&ctx, app).into_iter().find(|(_, n)| n.label().is_some_and(|label| label.starts_with(&prefix))).unwrap().1;
            click(&ctx, app, &stop);
        };
        let check = |app: &mut App, value, disabled| {
            let picker = picker(app, "Ease");
            assert_eq!(picker.value(), Some(value));
            assert_eq!(picker.is_disabled(), disabled);
        };
        let header = named(&mut app, "Easing");
        click(&ctx, &mut app, &header);
        choose(&mut app, "Ease", "Back Out");
        choose(&mut app, "Easing mode", "Per Segment");
        check(&mut app, "Linear", false);
        choose(&mut app, "Ease", "Quad Out");

        let track = named(&mut app, "Array: add a property stop");
        click(&ctx, &mut app, &track);
        let inserted = app.array_panel.settings.clone().unwrap();
        let marker = inserted.stops[1].marker_id();
        assert_eq!(app.array_panel.selected_stop, Some(marker));
        assert_eq!(inserted.stops[1].value.outbound, Some(Easing::QuadOut));
        check(&mut app, "Quad Out", false);
        history_key(&ctx, &mut app, Key::Z, Modifiers::NONE);
        assert_eq!(app.array_panel.settings.as_ref().unwrap().stops.len(), 2);
        history_key(&ctx, &mut app, Key::Y, Modifiers::NONE);
        assert_eq!(app.array_panel.settings.as_ref(), Some(&inserted));
        choose(&mut app, "Ease", "Sine In");
        history_key(&ctx, &mut app, Key::Z, Modifiers::NONE);
        check(&mut app, "Quad Out", false);
        history_key(&ctx, &mut app, Key::Y, Modifiers::NONE);
        check(&mut app, "Sine In", false);
        select(&mut app, 1);
        check(&mut app, "Quad Out", false);
        select(&mut app, 3);
        check(&mut app, "Linear", true);
        assert!(!named(&mut app, "Alpha").is_disabled());
        choose(&mut app, "Easing mode", "Global");
        check(&mut app, "Back Out", false);
        choose(&mut app, "Ease", "Bounce Out");
        choose(&mut app, "Easing mode", "Per Segment");
        check(&mut app, "Linear", true);
        select(&mut app, 2);
        check(&mut app, "Sine In", false);
        let remove = named(&mut app, "Remove array stop");
        click(&ctx, &mut app, &remove);
        assert_eq!(app.array_panel.settings.as_ref().unwrap().stops[0].value.outbound, Some(Easing::QuadOut));
        check(&mut app, "Linear", true);
        history_key(&ctx, &mut app, Key::Z, Modifiers::NONE);
        assert_eq!(app.array_panel.selected_stop, Some(marker));
        check(&mut app, "Sine In", false);
        history_key(&ctx, &mut app, Key::Y, Modifiers::NONE);
        check(&mut app, "Linear", true);
        assert_eq!(app.array_panel.settings.as_ref().unwrap().easing.preset, Easing::BounceOut);
    }

    #[test]
    fn quitting_resolves_array_before_the_normal_document_save_flow() {
        for (choice, dirty, apply, cancel_save) in [
            ("Keep Editing", false, false, false),
            ("Discard Array and Quit", false, false, false),
            ("Discard Array and Quit", true, false, true),
            ("Apply Array and Quit", false, true, true),
            ("Apply Array and Quit", false, true, false),
        ] {
            let (mut app, ctx) = linear_app();
            if !dirty { app.s.mark_saved(); }
            let (original, steps) = (app.s.doc.clone(), app.s.history_len());
            app.array_panel.select_mode(&mut app.s, Some(Mode::Linear));
            let mut ends = app.array_panel.gizmo_endpoints().unwrap();
            ends.end[0] += 40.0;
            app.array_panel.set_endpoints(&mut app.s, ends);
            let (preview, draft) = (app.s.doc.clone(), app.array_panel.settings.clone());
            nodes(&ctx, &mut app);
            if choice == "Keep Editing" {
                app.shot = None;
                let mut input = egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(Pos2::ZERO, vec2(1600.0, 940.0))),
                    ..Default::default()
                };
                input.viewports.entry(egui::ViewportId::ROOT).or_default().events.push(egui::ViewportEvent::Close);
                let mut output = ctx.run_ui(input, |ui| app.frame(ui));
                output.textures_delta.clear();
                assert!(output.viewport_output.values().any(|viewport| viewport.commands.contains(&egui::ViewportCommand::CancelClose)));
            } else if apply {
                history_key(&ctx, &mut app, Key::Q, Modifiers::NONE);
            } else if dirty && crate::platform::OS != crate::platform::Os::Mac && !crate::platform::WEB {
                frame(&ctx, &mut app, vec![Event::Key {
                    key: Key::F, physical_key: None, pressed: true, repeat: false, modifiers: Modifiers::ALT,
                }]);
                let menu = nodes(&ctx, &mut app);
                let exit = menu.iter().find(|(_, n)| n.label().is_some_and(|label| label.starts_with("Exit")))
                    .unwrap_or_else(|| panic!("Exit menu item missing: {:?}", menu.iter().filter_map(|(_, n)| n.label()).collect::<Vec<_>>())).1.clone();
                click(&ctx, &mut app, &exit);
            } else {
                app.system_request(crate::platform::SystemRequest::Quit);
            }
            assert!(app.array_quit_pending && app.s.array_preview_active() && !app.tabs.asking());
            let button = nodes(&ctx, &mut app).into_iter().find(|(_, n)| n.label() == Some(choice)).unwrap().1;
            click(&ctx, &mut app, &button);
            assert!(!app.array_quit_pending);
            if choice == "Keep Editing" {
                assert!(app.s.array_preview_active() && app.array_panel.can_step(false));
                assert!(app.s.doc == preview && app.array_panel.settings == draft);
                assert!(!app.allow_close && app.tabs.closing.is_none());
                continue;
            }
            assert!(!app.s.array_preview_active());
            assert_eq!(app.s.history_len(), steps + usize::from(apply));
            assert!(app.s.doc == if apply { preview } else { original });
            if dirty || apply {
                assert!(app.tabs.asking() && !app.allow_close);
                if !cancel_save {
                    let discard = nodes(&ctx, &mut app).into_iter().find(|(_, n)| n.label() == Some("Don't Save")).unwrap().1;
                    click(&ctx, &mut app, &discard);
                    assert!(app.allow_close && app.tabs.closing.is_none());
                    continue;
                }
                let cancel = nodes(&ctx, &mut app).into_iter().find(|(_, n)| n.label() == Some("Cancel")).unwrap().1;
                click(&ctx, &mut app, &cancel);
                assert!(!app.allow_close && app.tabs.closing.is_none() && !app.modal_open());
                assert!(!app.s.array_preview_active());
                if apply {
                    assert!(app.array_panel.settings.is_none());
                } else {
                    assert_eq!(app.array_panel.settings, draft);
                }
            } else {
                assert!(app.allow_close);
            }
        }
    }

    #[test]
    fn array_history_exits_keep_the_draft_and_leave_document_redo_intact() {
        let (mut app, ctx) = linear_app();
        app.s.execute("element.translate", &json!({"dx": 10, "dy": 0})).unwrap();
        app.s.undo().unwrap();
        let (base, source, undo_len, redo_len) = (app.s.doc.clone(), app.s.selection.clone(), app.s.history_len(), app.s.history().redo_len());
        for exit in ["Cancel", "Escape", "None"] {
            app.array_panel.select_mode(&mut app.s, Some(Mode::Linear));
            assert!(!app.array_panel.can_step(false) && !app.array_panel.can_step(true));
            nodes(&ctx, &mut app);
            let mut ends = app.array_panel.gizmo_endpoints().unwrap();
            ends.end[0] -= 40.0;
            app.array_panel.set_endpoints(&mut app.s, ends);
            let draft = app.array_panel.settings.clone();
            ends.end[1] += 10.0;
            app.array_panel.set_endpoints(&mut app.s, ends);
            history_key(&ctx, &mut app, Key::Z, Modifiers::NONE);
            assert_eq!(app.array_panel.settings, draft);
            assert!(app.array_panel.can_step(true));
            match exit {
                "Cancel" => {
                    let cancel = nodes(&ctx, &mut app).into_iter().find(|(_, n)| n.label() == Some("Cancel")).unwrap().1;
                    click(&ctx, &mut app, &cancel);
                }
                "Escape" => {
                    frame(&ctx, &mut app, vec![key(Key::Escape, true)]);
                    frame(&ctx, &mut app, vec![key(Key::Escape, false)]);
                }
                _ => app.array_panel.select_mode(&mut app.s, None),
            }
            assert!(!app.s.array_preview_active());
            assert!(app.s.doc == base && app.s.selection == source);
            assert_eq!(app.array_panel.settings, draft);
            assert_eq!(app.s.history_len(), undo_len);
            assert_eq!(app.s.history().redo_len(), redo_len);
        }
    }

    #[test]
    fn transform_stop_fields_drag_live_and_type_without_overwriting_other_stops() {
        let (mut app, ctx) = linear_app();
        app.array_panel.select_mode(&mut app.s, Some(Mode::Linear));
        let field = |app: &mut App, label: &str| {
            nodes(&ctx, app).into_iter().find(|(_, n)| n.label() == Some(label)).unwrap()
        };
        let value = |app: &App, i: usize| app.array_panel.settings.as_ref().unwrap().stops.get(i).unwrap().value.clone();
        let last = |app: &App| app.s.selected_elements().last().unwrap().1.transform;
        assert_eq!(value(&app, 0), ArrayPropertyStop::new(0.0, 1.0));
        let before = last(&app);
        let lock = field(&mut app, "Lock scale").1;
        click(&ctx, &mut app, &lock);
        let scale = field(&mut app, "Scale X").1.bounds().unwrap();
        let from = pos2(((scale.x0 + scale.x1) * 0.5) as f32, ((scale.y0 + scale.y1) * 0.5) as f32);
        drag_gizmo(&ctx, &mut app, from, vec2(40.0, 0.0), false);
        let first = value(&app, 0);
        assert!(first.scale[0] > 1.0);
        assert_eq!(first.scale[0], first.scale[1]);
        assert_eq!(last(&app), before);
        assert!(app.s.selected_elements().first().unwrap().1.transform.m[0] > 1.0);
        frame(&ctx, &mut app, vec![pointer_button(from + vec2(40.0, 0.0), false)]);

        let other = nodes(&ctx, &mut app).into_iter().find(|(_, n)| n.label().is_some_and(|s| s.starts_with("Array stop 2 of"))).unwrap().1;
        click(&ctx, &mut app, &other);
        assert_eq!(field(&mut app, "Scale X").1.numeric_value(), Some(100.0));
        let rotation = field(&mut app, "Rotation").1;
        click(&ctx, &mut app, &rotation);
        frame(&ctx, &mut app, vec![]);
        frame(&ctx, &mut app, vec![Event::Key { key: Key::A, physical_key: None, pressed: true, repeat: false,
            modifiers: Modifiers::COMMAND }, Event::Text("720".to_owned())]);
        frame(&ctx, &mut app, vec![key(Key::Enter, true)]);
        assert_eq!(value(&app, 1).rotation, 720.0);
        assert_eq!(value(&app, 0), first);
        assert_eq!(value(&app, 1).alpha, 1.0);

        let transform = field(&mut app, "Transform").1;
        click(&ctx, &mut app, &transform);
        let collapsed = nodes(&ctx, &mut app);
        assert!(!collapsed.iter().any(|(_, n)| n.label() == Some("Rotation")));
        assert!(collapsed.iter().any(|(_, n)| n.role() == Role::SpinButton && n.label() == Some("Alpha")));

        let mut endpoints = app.array_panel.gizmo_endpoints().unwrap();
        endpoints.end[0] += 150.0;
        app.array_panel.set_endpoints(&mut app.s, endpoints);
        app.array_panel.select_mode(&mut app.s, Some(Mode::Radial));
        let mut radial = app.array_panel.settings.as_ref().unwrap().radial;
        radial.closed_loop = false;
        radial.spiral_offset = 44.0;
        radial.centre_drift = [20.0, -30.0];
        radial.sweep = -810.0;
        radial.rotate = true;
        app.array_panel.set_gizmo_geometry(&mut app.s, Geometry::Radial(radial), None);
        assert_eq!(value(&app, 0), first);
        assert_eq!(value(&app, 1), ArrayPropertyStop { rotation: 720.0, ..ArrayPropertyStop::new(1.0, 1.0) });
    }

    fn near(actual: [f32; 2], expected: [f32; 2]) {
        assert!((actual[0] - expected[0]).abs() < 0.01 && (actual[1] - expected[1]).abs() < 0.01, "{actual:?} != {expected:?}");
    }

    fn preview_ends(app: &App, ends: Endpoints) {
        let selected = app.s.selected_elements();
        let point = |e: &(_, std::sync::Arc<anicraft_engine::doc::Element>)| [e.1.transform.m[4], e.1.transform.m[5]];
        near(point(selected.first().unwrap()), ends.start);
        near(point(selected.last().unwrap()), ends.end);
    }

    #[test]
    fn changing_selection_discards_a_cancelled_draft_even_when_the_panel_is_closed() {
        let (mut app, ctx) = linear_app();
        let (_, first) = app.s.selection.first().unwrap();
        let origin = source_origin(&app.s).unwrap();
        instance(&mut app.s, 475.0, "Second");
        let (_, second) = app.s.selection.first().unwrap();
        app.s.select_only([first]);
        app.array_panel.select_mode(&mut app.s, Some(Mode::Radial));
        let settings = app.array_panel.settings.as_mut().unwrap();
        settings.count = 17;
        settings.radial.closed_loop = false;
        settings.radial.sweep = -720.0;
        settings.radial.spiral_offset = 35.0;
        app.array_panel.refresh(&mut app.s);
        app.array_panel.dim_stage = false;
        app.array_panel.show_gizmo = false;
        let fields = nodes(&ctx, &mut app);
        click(&ctx, &mut app, &fields.iter().find(|(_, n)| n.label() == Some("Cancel")).unwrap().1);
        assert_eq!(app.array_panel.settings.as_ref().unwrap().radial.spiral_offset, 35.0);

        crate::dock_ui::apply(&mut app, vec![crate::dock_ui::Act::Close(Panel::Array)]);
        app.s.select_only([second]);
        frame(&ctx, &mut app, vec![]);
        assert!(app.array_panel.settings.is_none());
        app.s.select_only([first]);
        app.array_panel.select_mode(&mut app.s, Some(Mode::Radial));
        assert_eq!(app.array_panel.settings.as_ref(), Some(&Settings::at(origin)));
        assert!(!app.array_panel.dim_stage() && !app.array_panel.shows_registrations());
    }

    #[test]
    fn radial_panel_keeps_winding_in_open_mode_and_limits_a_closed_loop() {
        let (mut app, ctx) = linear_app();
        app.array_panel.select_mode(&mut app.s, Some(Mode::Radial));
        let set_field = |app: &mut App, label: &str, value: f64| {
            let fields = nodes(&ctx, app);
            let input = fields.iter().find(|(_, n)| n.role() == Role::SpinButton && n.label() == Some(label)).unwrap().0;
            set_number(&ctx, app, input, value);
        };
        let toggle_loop = |app: &mut App| {
            let shown = nodes(&ctx, app);
            click(&ctx, app, &shown.iter().find(|(_, n)| n.label() == Some("Closed Loop")).unwrap().1);
        };
        let shows_offset = |app: &mut App| nodes(&ctx, app).iter().any(|(_, n)| n.value() == Some("Spiral Offset"));
        assert!(app.array_panel.settings.as_ref().unwrap().radial.closed_loop);
        assert!(!shows_offset(&mut app));
        toggle_loop(&mut app);
        assert!(shows_offset(&mut app));
        set_field(&mut app, "Spiral Offset", 15.0);
        set_field(&mut app, "Centre Drift X", 32.0);
        set_field(&mut app, "Centre Drift Y", -18.0);
        for sweep in [358.0, 360.0, 362.0, -360.0, 720.0, -1080.0] {
            set_field(&mut app, "Sweep", sweep);
            let radial = &app.array_panel.settings.as_ref().unwrap().radial;
            assert_eq!(radial.sweep, sweep as f32);
            assert!(!radial.closed_loop);
            let last = app.s.selected_elements().last().unwrap().1.transform.translation();
            let [x, y] = radial.centre_after_turns(f64::from(radial.sweep.abs()) / 360.0);
            let radius = (last.x - x as f32).hypot(last.y - y as f32);
            assert!((radius - radial.radius - 15.0 * radial.sweep.abs() / 360.0).abs() < 0.001);
        }
        toggle_loop(&mut app);
        assert!(!shows_offset(&mut app));
        assert!(!nodes(&ctx, &mut app).iter().any(|(_, n)| n.label() == Some("Centre Drift X")));
        let radial = &app.array_panel.settings.as_ref().unwrap().radial;
        assert!(radial.closed_loop);
        assert_eq!(radial.sweep, -360.0);
        toggle_loop(&mut app);
        assert_eq!(app.array_panel.settings.as_ref().unwrap().radial.spiral_offset, 15.0);
        assert_eq!(app.array_panel.settings.as_ref().unwrap().radial.centre_drift, [32.0, -18.0]);
    }

    #[test]
    fn radial_stage_handles_update_fields_and_keep_cancel_commit_and_undo() {
        use anicraft_engine::geom::Vec2 as Point;
        let (mut app, ctx) = linear_app();
        let doc = app.s.doc.clone();
        let selection = app.s.selection.clone();
        let steps = app.s.history_len();
        app.array_panel.select_mode(&mut app.s, Some(Mode::Radial));
        let radial = Radial { centre: [260.0, 220.0], radius: 80.0, start: 0.0, sweep: 90.0,
            closed_loop: false, spiral_offset: 0.0, centre_drift: [70.0, -90.0], rotate: false };
        app.array_panel.set_gizmo_geometry(&mut app.s, Geometry::Radial(radial), None);
        nodes(&ctx, &mut app);

        let current = |app: &App| app.array_panel.settings.as_ref().unwrap().radial;
        let drag_point = |app: &mut App, from: Point, delta: Vec2| {
            let pos = screen(app, [from.x, from.y]);
            let zoom = app.view.zoom;
            drag_gizmo(&ctx, app, pos, delta * zoom, true);
        };
        drag_point(&mut app, Point::new(260.0, 220.0), vec2(28.0, -15.0));
        let shifted = current(&app);
        near(shifted.centre, [288.0, 205.0]);
        drag_point(&mut app, shifted.point(0.5), vec2(20.0, 35.0));
        let shifted = current(&app);
        near(shifted.centre, [308.0, 240.0]);
        drag_point(&mut app, shifted.point(0.0), vec2(0.0, -60.0));
        let started = current(&app);
        assert!((started.radius - 100.0).abs() < 0.01 && (started.start + 36.8699).abs() < 0.01);
        assert_eq!(started.sweep, radial.sweep);

        let [x, y] = started.centre_after_turns(1.0);
        drag_point(&mut app, Point::new(x as f32, y as f32), vec2(25.0, 20.0));
        let drifting = current(&app);
        near(drifting.centre_drift, [95.0, -70.0]);
        assert_eq!(drifting.spiral_offset, started.spiral_offset);
        assert_eq!(drifting.sweep, started.sweep);

        let [x, y] = drifting.centre_after_turns(0.5);
        drag_point(&mut app, Point::new(x as f32, y as f32), vec2(-12.0, 16.0));
        let translated = current(&app);
        near(translated.centre, [296.0, 256.0]);
        assert_eq!(translated.centre_drift, drifting.centre_drift);

        let from = translated.point(1.0);
        let target = Radial { sweep: 120.0, spiral_offset: 60.0, ..translated }.point(1.0);
        drag_point(&mut app, from, vec2(target.x - from.x, target.y - from.y));
        let ended = current(&app);
        assert!((ended.sweep - 120.0).abs() < 0.01 && (ended.spiral_offset - 60.0).abs() < 0.05);
        assert_eq!(ended.centre_drift, translated.centre_drift);
        let last = app.s.selected_elements().last().unwrap().1.transform.translation();
        near([last.x, last.y], [target.x, target.y]);

        let fields = nodes(&ctx, &mut app);
        let (_, offset) = fields.iter().find(|(_, n)| n.label() == Some("Spiral Offset")).unwrap();
        assert!((offset.numeric_value().unwrap() - 60.0).abs() < 0.05);
        let (_, drift_x) = fields.iter().find(|(_, n)| n.label() == Some("Centre Drift X")).unwrap();
        assert!((drift_x.numeric_value().unwrap() - 95.0).abs() < 0.05);
        click(&ctx, &mut app, &fields.iter().find(|(_, n)| n.label() == Some("Cancel")).unwrap().1);
        assert!(app.s.doc == doc && app.s.selection == selection);
        assert_eq!(app.s.history_len(), steps);
        app.array_panel.select_mode(&mut app.s, Some(Mode::Radial));
        assert_eq!(current(&app), ended);
        let fields = nodes(&ctx, &mut app);
        click(&ctx, &mut app, &fields.iter().find(|(_, n)| n.label() == Some("Commit")).unwrap().1);
        assert!(app.array_panel.gizmo_geometry().is_none());
        assert_eq!(app.s.history_len(), steps + 1);
        app.s.undo().unwrap();
        assert!(app.s.doc == doc && app.s.selection == selection);
    }

    #[test]
    fn stage_drags_change_endpoints_and_numeric_fields_through_the_same_preview() {
        let (mut app, ctx) = linear_app();
        let (doc, selection, steps) = (app.s.doc.clone(), app.s.selection.clone(), app.s.history_len());
        app.array_panel.select_mode(&mut app.s, Some(Mode::Linear));
        nodes(&ctx, &mut app);
        let initial = app.array_panel.gizmo_endpoints().unwrap();
        let zoom = app.view.zoom;
        let start = screen(&app, initial.start);
        drag_gizmo(&ctx, &mut app, start, vec2(18.0, 24.0) * zoom, false);
        let moved_start = app.array_panel.gizmo_endpoints().unwrap();
        near(moved_start.start, [initial.start[0] + 18.0, initial.start[1] + 24.0]);
        assert_eq!(moved_start.end, initial.end);
        preview_ends(&app, moved_start);
        let released = screen(&app, moved_start.start);
        frame(&ctx, &mut app, vec![pointer_button(released, false)]);

        let end = screen(&app, moved_start.end);
        drag_gizmo(&ctx, &mut app, end, vec2(32.0, 18.0) * zoom, true);
        let moved_end = app.array_panel.gizmo_endpoints().unwrap();
        near(moved_end.end, [initial.end[0] + 32.0, initial.end[1] + 18.0]);
        assert_eq!(moved_end.start, moved_start.start);

        let mid = screen(&app, moved_end.start).lerp(screen(&app, moved_end.end), 0.5) + vec2(0.0, 8.0);
        drag_gizmo(&ctx, &mut app, mid, vec2(20.0, 30.0) * zoom, true);
        let moved_bar = app.array_panel.gizmo_endpoints().unwrap();
        near(moved_bar.start, [moved_end.start[0] + 20.0, moved_end.start[1] + 30.0]);
        near(moved_bar.end, [moved_end.end[0] + 20.0, moved_end.end[1] + 30.0]);
        preview_ends(&app, moved_bar);

        let fields = nodes(&ctx, &mut app);
        let (end_x, node) = fields.iter().find(|(_, n)| n.label() == Some("End X")).unwrap();
        assert!((node.numeric_value().unwrap() - f64::from(moved_bar.end[0])).abs() < 0.01);
        set_number(&ctx, &mut app, *end_x, 620.0);
        let typed = app.array_panel.gizmo_endpoints().unwrap();
        assert_eq!(typed.end[0], 620.0);
        preview_ends(&app, typed);

        // Leave room for the full distribution section in this dock viewport.
        let stops = fields.iter().find(|(_, node)| node.label() == Some("Property stops")).unwrap().1.clone();
        click(&ctx, &mut app, &stops);
        let fields = nodes(&ctx, &mut app);
        let count = fields.iter().find(|(_, n)| n.label() == Some("Instances") && n.numeric_value() == Some(10.0))
            .unwrap_or_else(|| panic!("Instances control missing from Array accessibility tree: {:?}", fields.iter()
                .filter_map(|(_, n)| n.label().map(|label| (n.role(), label, n.numeric_value()))).collect::<Vec<_>>())).0;
        set_number(&ctx, &mut app, count, 1.0);
        assert_eq!(app.s.selection.len(), 1);
        assert_eq!(app.array_panel.gizmo_endpoints(), Some(typed));
        assert_eq!(typed.spacing(1), None);
        set_number(&ctx, &mut app, count, 5.0);
        assert_eq!(app.s.selection.len(), 5);
        assert_eq!(app.array_panel.gizmo_endpoints(), Some(typed));
        preview_ends(&app, typed);

        let shown = nodes(&ctx, &mut app);
        click(&ctx, &mut app, &shown.iter().find(|(_, n)| n.label() == Some("Commit")).unwrap().1);
        assert_eq!(app.s.history_len(), steps + 1);
        assert!(app.array_panel.gizmo_endpoints().is_none());
        app.s.undo().unwrap();
        assert!(app.s.doc == doc && app.s.selection == selection);
    }

    #[test]
    fn commit_resets_configuration_at_the_next_symbols_current_position() {
        let (mut app, ctx) = linear_app();
        let (_, first) = app.s.selection.first().unwrap();
        instance(&mut app.s, 530.0, "Second");
        let (_, second) = app.s.selection.first().unwrap();
        let original_origin = source_origin(&app.s).unwrap();
        app.s.select_only([first]);
        app.array_panel.select_mode(&mut app.s, Some(Mode::Linear));
        let config = app.array_panel.settings.as_mut().unwrap();
        config.count = 4;
        config.linear = Endpoints { start: [73.0, 222.0], end: [381.0, 129.0] };
        config.radial.radius = 67.0;
        config.backwards = true;
        config.stops.insert(1, PropertyStop::new(2, 0.25, 0.4));
        app.array_panel.refresh(&mut app.s);

        for label in ["Hide Array Handles", "Dim Stage", "Commit"] {
            let shown = nodes(&ctx, &mut app);
            click(&ctx, &mut app, &shown.iter().find(|(_, n)| n.label() == Some(label)).unwrap().1);
        }
        assert_eq!(app.s.selection.len(), 4);
        assert_eq!(app.array_panel.mode(), None);
        assert!(app.array_panel.settings.is_none() && !app.s.array_preview_active());

        app.s.select_only([second]);
        app.s.execute("element.translate", &json!({"dx": 42, "dy": 32})).unwrap();
        let moved_origin = [original_origin[0] + 42.0, original_origin[1] + 32.0];
        app.array_panel.select_mode(&mut app.s, Some(Mode::Linear));
        assert_eq!(app.array_panel.settings.as_ref(), Some(&Settings::at(moved_origin)));
        assert_eq!(app.s.selection.len(), 10);
        assert_eq!(app.s.selection.first().unwrap().1, second);
        preview_ends(&app, Settings::at(moved_origin).linear);
        assert!(!app.array_panel.dim_stage() && !app.array_panel.shows_registrations());
    }

    #[test]
    fn direction_length_and_each_spacing_field_edit_the_canonical_end() {
        let (mut app, ctx) = linear_app();
        app.array_panel.select_mode(&mut app.s, Some(Mode::Linear));
        let start = app.array_panel.gizmo_endpoints().unwrap().start;
        let input = |ctx: &egui::Context, app: &mut App, label: &str, value: f64| {
            let target = nodes(ctx, app).into_iter().find(|(_, n)| n.label() == Some(label)).unwrap().0;
            set_number(ctx, app, target, value);
            let ends = app.array_panel.gizmo_endpoints().unwrap();
            assert_eq!(ends.start, start);
            ends
        };
        near(input(&ctx, &mut app, "Direction", 90.0).extent(), [0.0, 324.0]);
        near(input(&ctx, &mut app, "Length", 90.0).extent(), [0.0, 90.0]);
        near(input(&ctx, &mut app, "Spacing X", 7.0).extent(), [63.0, 90.0]);
        let ends = input(&ctx, &mut app, "Spacing Y", -5.0);
        near(ends.extent(), [63.0, -45.0]);
        preview_ends(&app, ends);

        app.array_panel.settings.as_mut().unwrap().count = 1;
        app.array_panel.refresh(&mut app.s);
        let fields = nodes(&ctx, &mut app);
        for label in ["Spacing X", "Spacing Y"] {
            assert!(fields.iter().find(|(_, n)| n.label() == Some(label)).unwrap().1.is_disabled());
        }
        near(input(&ctx, &mut app, "Length", 0.0).extent(), [0.0, 0.0]);
        let fields = nodes(&ctx, &mut app);
        assert!(fields.iter().find(|(_, n)| n.label() == Some("Direction")).unwrap().1.is_disabled());
        near(input(&ctx, &mut app, "Length", 36.0).extent(), [36.0, 0.0]);
        assert_eq!(app.s.selection.len(), 1);
    }

    #[test]
    fn radial_visibility_toggle_controls_instance_registrations() {
        let (mut app, ctx) = linear_app();
        app.array_panel.select_mode(&mut app.s, Some(Mode::Radial));
        let fields = nodes(&ctx, &mut app);
        assert!(app.array_panel.shows_registrations() && matches!(app.array_panel.gizmo_geometry(), Some(Geometry::Radial(_))));
        assert_eq!(app.s.selected_elements().len(), 10);
        click(&ctx, &mut app, &fields.iter().find(|(_, n)| n.label() == Some("Hide Array Handles")).unwrap().1);
        assert!(!app.array_panel.shows_registrations());
        assert!(app.array_panel.gizmo_geometry().is_none());
        assert!(app.s.array_preview_active() && app.s.selected_elements().len() == 10);
    }

    #[test]
    fn hiding_the_gizmo_keeps_preview_and_numeric_editing_and_exit_restores_the_source() {
        let (mut app, ctx) = linear_app();
        let (doc, selection, steps) = (app.s.doc.clone(), app.s.selection.clone(), app.s.history_len());
        app.array_panel.select_mode(&mut app.s, Some(Mode::Linear));
        let shown = nodes(&ctx, &mut app);
        let ends = app.array_panel.gizmo_endpoints().unwrap();
        let preview = app.s.doc.clone();
        click(&ctx, &mut app, &shown.iter().find(|(_, n)| n.label() == Some("Hide Array Handles")).unwrap().1);
        assert!(app.array_panel.gizmo_endpoints().is_none() && app.s.array_preview_active() && app.s.doc == preview);
        let start = screen(&app, ends.start);
        drag_gizmo(&ctx, &mut app, start, vec2(40.0, 30.0), true);
        assert!(app.s.doc == preview);
        let shown = nodes(&ctx, &mut app);
        let (end_y, _) = shown.iter().find(|(_, n)| n.label() == Some("End Y")).unwrap();
        set_number(&ctx, &mut app, *end_y, 129.0);
        assert!(app.s.doc != preview && app.s.array_preview_active());

        let shown = nodes(&ctx, &mut app);
        click(&ctx, &mut app, &shown.iter().find(|(_, n)| n.label() == Some("Show Array Handles")).unwrap().1);
        let ends = app.array_panel.gizmo_endpoints().unwrap();
        assert_eq!(ends.end[1], 129.0);
        preview_ends(&app, ends);
        let start = screen(&app, ends.start);
        drag_gizmo(&ctx, &mut app, start, vec2(40.0, 30.0), false);
        assert!(app.array_panel.gizmo_drag.is_some());
        let recovery = app.array_panel.settings.clone();
        frame(&ctx, &mut app, vec![key(Key::Escape, true)]);
        assert!(app.array_panel.gizmo_drag.is_none() && !app.s.array_preview_active());
        assert_eq!(app.array_panel.settings, recovery);
        frame(&ctx, &mut app, vec![pointer_button(start + vec2(40.0, 30.0), false), key(Key::Escape, false)]);
        assert!(app.s.doc == doc && app.s.selection == selection);

        app.array_panel.select_mode(&mut app.s, Some(Mode::Linear));
        assert_eq!(app.array_panel.settings, recovery);
        app.array_panel.set_endpoints(&mut app.s, ends);
        let recovery = app.array_panel.settings.clone();
        let shown = nodes(&ctx, &mut app);
        click(&ctx, &mut app, &shown.iter().find(|(_, n)| n.label() == Some("Cancel")).unwrap().1);
        assert_eq!(app.array_panel.settings, recovery);
        assert!(app.s.doc == doc && app.s.selection == selection);
        assert_eq!(app.s.history_len(), steps);
    }

    #[test]
    fn real_controls_enter_preview_and_escape_gives_child_controls_priority() {
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        let mut s = Session::default();
        instance(&mut s, 50.0, "Source");
        let (doc, selection, steps) = (s.doc.clone(), s.selection.clone(), s.history_len());
        let mut app = App::with_context(&ctx, s, Some(std::path::PathBuf::from("unused.png")));
        app.show_panel(Panel::Array);
        let combo = |nodes: &[(NodeId, Node)], value| {
            nodes.iter().find(|(_, n)| n.role() == Role::ComboBox && n.value() == Some(value) && n.label() == Some("Array type")).unwrap().1.clone()
        };
        let named = |nodes: &[(NodeId, Node)], label| nodes.iter().find(|(_, n)| n.label() == Some(label)).unwrap().1.clone();
        let idle = nodes(&ctx, &mut app);
        assert!(app.s.doc == doc && !app.s.array_preview_active());
        click(&ctx, &mut app, &combo(&idle, "None"));
        assert!(egui::Popup::is_any_open(&ctx), "the Array type dropdown opened");
        let choices = nodes(&ctx, &mut app);
        for label in ["Grid", "Along Path"] {
            assert!(named(&choices, label).is_disabled(), "{label} is unavailable");
        }
        click(&ctx, &mut app, &named(&choices, "Linear"));
        assert_eq!(app.s.selection.len(), 10, "changing the dropdown previews immediately");
        let shown = nodes(&ctx, &mut app);
        assert!(shown.iter().any(|(_, n)| n.label() == Some("Colour Effect")));
        click(&ctx, &mut app, &combo(&shown, "Linear"));
        assert!(egui::Popup::is_any_open(&ctx));
        frame(&ctx, &mut app, vec![key(Key::Escape, true)]);
        assert!(app.s.array_preview_active() && !egui::Popup::is_any_open(&ctx));

        frame(&ctx, &mut app, vec![key(Key::Escape, false)]);
        let shown = nodes(&ctx, &mut app);
        click(&ctx, &mut app, &named(&shown, "Array stop position"));
        nodes(&ctx, &mut app);
        assert!(ctx.text_edit_focused());
        frame(&ctx, &mut app, vec![key(Key::Escape, true)]);
        assert!(app.s.array_preview_active() && !ctx.text_edit_focused());

        app.show_panel(Panel::Properties);
        nodes(&ctx, &mut app);
        assert!(app.s.array_preview_active() && !app.panel_visible(Panel::Array));
        crate::dock_ui::apply(&mut app, vec![crate::dock_ui::Act::Close(Panel::Array)]);
        let hidden = nodes(&ctx, &mut app);
        assert!(app.s.array_preview_active() && !app.panel_visible(Panel::Array));
        assert!(hidden.iter().any(|(_, n)| n.label() == Some("Show Array")));
        assert!(app.run("edit.delete", json!({})).is_none());
        assert!(app.perform("file.new", json!({})).is_none());
        frame(&ctx, &mut app, vec![key(Key::Escape, false)]);
        frame(&ctx, &mut app, vec![key(Key::Escape, true)]);
        assert!(app.s.doc == doc && app.s.selection == selection && !app.s.in_gesture());
        assert_eq!(app.s.history_len(), steps);
        assert_eq!(app.array_panel.mode(), None);
    }
}
