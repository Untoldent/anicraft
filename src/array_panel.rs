//! The Array panel retains unfinished settings for recovery after cancellation.

use crate::a11y::Named;
use crate::app::{App, Drag};
use crate::array_gizmo::{Endpoints, GizmoDrag};
use crate::colour_panel::PropertyStop;
use crate::dock::{Panel, Place};
use crate::theme::{FORM_LABEL_DOCK, Section, T, Type, form_row, section, text, with_type};
use anicraft_engine::{CLOSED_SWEEP_LIMIT, Session};
use anicraft_engine::doc::{ElementId, ElementKind, LayerId};
use egui::{Id, Key, Modifiers, Ui};
use serde_json::{Value, json};

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
}

#[derive(Clone, Debug, PartialEq)]
struct Radial {
    centre: [f32; 2],
    radius: f32,
    start: f32,
    sweep: f32,
    closed_loop: bool,
    spiral_offset: f32,
    rotate: bool,
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
                closed_loop: true, spiral_offset: 0.0, rotate: false,
            },
            backwards: false,
            stops: vec![PropertyStop::new(0, 0.0, 1.0), PropertyStop::new(1, 1.0, 1.0)],
        }
    }

    fn params(&self, mode: Mode) -> Value {
        let alpha_stops: Vec<Value> = self
            .stops
            .iter()
            .map(|s| {
                json!({
                    "offset": s.value.offset, "alpha": s.value.alpha,
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
                "spiral_offset": self.radial.spiral_offset, "rotate": self.radial.rotate,
            }),
        };
        params["stacking"] = json!(if self.backwards { "front_to_back" } else { "back_to_front" });
        params["alpha_stops"] = json!(alpha_stops);
        params
    }
}

struct Active {
    mode: Mode,
    /// Where defaults are anchored if settings are reset during this preview.
    origin: [f32; 2],
}

pub struct State {
    active: Option<Active>,
    settings: Option<Settings>,
    draft_source: Option<(LayerId, ElementId)>,
    error: Option<String>,
    reset_pending: bool,
    stop_editor_epoch: u64,
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
        if let Some(active) = &mut self.active {
            active.mode = mode;
            self.refresh(s);
        } else if let Some(origin) = source_origin(s) {
            let settings = self.settings.clone().unwrap_or_else(|| Settings::at(origin));
            match s.preview_array(&settings.params(mode)) {
                Ok(()) => {
                    self.active = Some(Active { mode, origin });
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
        self.discard_draft();
        self.settings = self.active.as_ref().map(|active| Settings::at(active.origin));
        self.gizmo_drag = None;
        self.refresh(s);
    }

    pub(crate) fn gizmo_endpoints(&self) -> Option<Endpoints> {
        if self.show_gizmo && self.mode() == Some(Mode::Linear) {
            self.settings.as_ref().map(|settings| settings.linear)
        } else {
            None
        }
    }

    pub(crate) fn shows_registrations(&self) -> bool {
        self.show_gizmo && self.active.is_some() && self.error.is_none()
    }

    pub(crate) fn gizmo_editable(&self) -> bool {
        !self.reset_pending
    }

    pub(crate) fn set_endpoints(&mut self, s: &mut Session, endpoints: Endpoints) {
        if self.mode() == Some(Mode::Linear)
            && let Some(settings) = &mut self.settings
            && settings.linear != endpoints
        {
            settings.linear = endpoints;
            self.refresh(s);
        }
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

fn text_focus_id(ctx: &egui::Context) -> Id {
    Id::new(("array_text_focus", ctx.viewport_id()))
}

pub(crate) fn remember_text_focus(ctx: &egui::Context) {
    let focus = ctx.memory(|m| m.focused()).filter(|_| ctx.text_edit_focused());
    let key = text_focus_id(ctx);
    ctx.data_mut(|d| d.insert_temp(key, focus));
}

pub(crate) fn finish_frame(app: &mut App, ctx: &egui::Context) {
    app.array_panel.check_draft(&app.s);
    if !app.panel_visible(Panel::Array) {
        app.array_panel.reset_pending = false;
    }
    remember_text_focus(ctx);
}

/// A live preview with a multi-stop alpha ramp and surrounding artwork for screenshot checks.
pub(crate) fn screenshot(app: &mut App, view: &str) {
    let open_radial = view.strip_prefix("array-radial-open:").and_then(|setup| {
        let (sweep, offset) = setup.split_once(',')?;
        Some((sweep.parse::<f32>().ok()?, offset.parse::<f32>().ok()?))
    });
    app.perform("file.new", json!({"width": 640, "height": 400}));
    app.run("draw.rect", json!({"x": 72, "y": 72, "w": 125, "h": 60, "fill": "#376b93", "stroke": null}));
    app.run("draw.ellipse", json!({"x": 494, "y": 275, "w": 82, "h": 68, "fill": "#376b93", "stroke": null}));
    let diameter = if open_radial.is_some() { 20 } else { 56 };
    app.run("draw.ellipse", json!({"x": 135, "y": 170, "w": diameter, "h": diameter, "fill": "#ce523f", "stroke": null}));
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
    let mode = if view == "array-radial" || open_radial.is_some() { Mode::Radial } else { Mode::Linear };
    app.array_panel.select_mode(&mut app.s, Some(mode));
    app.array_panel.show_gizmo = !matches!(view, "array-hidden" | "array-polish-off");
    app.array_panel.dim_stage = view != "array-polish-off";
    if let Some(settings) = &mut app.array_panel.settings {
        settings.linear.end[1] += 50.0;
        if mode == Mode::Radial {
            settings.radial.centre = [320.0, 200.0];
        }
        if let Some((sweep, offset)) = open_radial {
            settings.radial.closed_loop = false;
            settings.radial.sweep = sweep;
            settings.radial.spiral_offset = offset;
            if offset != 0.0 {
                settings.count = 24;
                settings.radial.radius = if offset > 0.0 { 38.0 } else { 140.0 };
            }
        } else {
            settings.stops =
                vec![PropertyStop::new(0, 0.0, 0.75), PropertyStop::new(2, 0.4, 1.0), PropertyStop::new(1, 1.0, 0.03)];
        }
    }
    app.array_panel.refresh(&mut app.s);
}

pub(crate) fn text_had_escape(ctx: &egui::Context) -> bool {
    // egui surrenders ordinary TextEdit focus before the frame on Escape. DragValue still
    // needs to see the key to discard the typed value rather than applying it on blur.
    let key = text_focus_id(ctx);
    let prior = ctx.data(|d| d.get_temp::<Option<Id>>(key)).flatten();
    ctx.text_edit_focused() || prior.is_some_and(|id| ctx.memory(|m| m.had_focus_last_frame(id)))
}

pub(crate) fn escape(app: &mut App, ctx: &egui::Context) {
    if !(app.s.array_preview_active() || app.array_panel.reset_pending)
        || app.modal_open()
        || app.palette.is_some()
        || app.dock_view.drag.is_some()
        || egui::Popup::is_any_open(ctx)
        || text_had_escape(ctx)
    {
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
        if state.mode().is_some()
            && let Some(settings) = &mut state.settings
        {
            let changed = ui
                .add_enabled_ui(!state.reset_pending, |ui| settings_ui(ui, settings, mode, state.stop_editor_epoch))
                .inner;
            if changed {
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
        ui.label(text(Type::Body, tr!("Reset the remembered Array geometry and property stops?")));
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

fn settings_ui(ui: &mut Ui, settings: &mut Settings, mode: Mode, stop_editor_epoch: u64) -> bool {
    let mut changed = false;
    section(ui, "array_stops", Section::Rule, tr!("Property stops"), None, |ui| {
        let id = ui.make_persistent_id(("array_stop_bar", stop_editor_epoch));
        changed |= crate::colour_panel::array_stop_bar(ui, id, &mut settings.stops, stop_properties);
    });
    ui.add_space(4.0);
    section(ui, "array_distribution", Section::Rule, tr!("Distribution"), None, |ui| {
        ui.horizontal_wrapped(|ui| {
            let label = ui.label(text(Type::Label, tr!("Instances")));
            changed |= with_type(ui, Type::Data, |ui| ui.add(egui::DragValue::new(&mut settings.count).range(1..=500)))
                .labelled_by(label.id)
                .changed();
        });
        match mode {
            Mode::Linear => {
                changed |= coordinates(ui, tr!("Start"), [tr!("Start X"), tr!("Start Y")], &mut settings.linear.start);
                changed |= coordinates(ui, tr!("End"), [tr!("End X"), tr!("End Y")], &mut settings.linear.end);
                changed |= linear_dimensions(ui, &mut settings.linear, settings.count);
            }
            Mode::Radial => changed |= radial_fields(ui, &mut settings.radial),
        }
        changed |= crate::theme::checkbox(ui, &mut settings.backwards, tr!("Stack first copy in front")).changed();
    });
    changed
}

fn radial_fields(ui: &mut Ui, radial: &mut Radial) -> bool {
    let mut changed = coordinates(ui, tr!("Centre"), [tr!("Centre X"), tr!("Centre Y")], &mut radial.centre);
    changed |= field(ui, tr!("Radius"), &mut radial.radius, " px", 0.0..=100_000.0);
    changed |= field(ui, tr!("Start angle"), &mut radial.start, " °", -100_000.0..=100_000.0);
    let loop_changed = crate::theme::checkbox(ui, &mut radial.closed_loop, tr!("Closed Loop")).changed();
    let limit = if radial.closed_loop { CLOSED_SWEEP_LIMIT } else { 100_000.0 };
    if loop_changed && radial.closed_loop {
        radial.sweep = radial.sweep.clamp(-limit, limit);
    }
    changed |= loop_changed;
    changed |= field(ui, tr!("Sweep"), &mut radial.sweep, " °", -limit..=limit);
    if !radial.closed_loop {
        changed |= field(ui, tr!("Spiral Offset"), &mut radial.spiral_offset, tr!(" px/turn"), -100_000.0..=100_000.0);
    }
    changed |= crate::theme::checkbox(ui, &mut radial.rotate, tr!("Rotate instances")).changed();
    changed
}

fn stop_properties(ui: &mut Ui, stop: &mut PropertyStop) -> bool {
    egui::Frame::new()
        .fill(T.panel_alt)
        .stroke(egui::Stroke::new(1.0, T.border))
        .corner_radius(T.radius_control)
        .inner_margin(egui::Margin::symmetric(8, 4))
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            // Add applicable editors here in order: Transform, Colour Effect, Filters,
            // Playback, Stacking Order. The property sections share the same outer card.
            section(ui, "array_stop_colour_effect", Section::Rule, tr!("Colour Effect"), None, |ui| {
                ui.horizontal_wrapped(|ui| {
                    let label = ui.label(text(Type::Label, tr!("Alpha")));
                    let mut value = stop.value.alpha * 100.0;
                    let changed = with_type(ui, Type::Data, |ui| {
                        ui.add(egui::DragValue::new(&mut value).range(0.0..=100.0).suffix(" %").max_decimals(1))
                    }).labelled_by(label.id).changed();
                    if changed {
                        stop.value.alpha = value / 100.0;
                    }
                    changed
                }).inner
            }).unwrap_or(false)
        }).inner
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

fn coordinates(ui: &mut Ui, label: &str, [x_name, y_name]: [&str; 2], [x, y]: &mut [f32; 2]) -> bool {
    form_row(ui, FORM_LABEL_DOCK, label, |ui| {
        let mut changed = false;
        for (value, axis, name) in [(x, "X ", x_name), (y, "Y ", y_name)] {
            changed |= with_type(ui, Type::Data, |ui| {
                ui.add(egui::DragValue::new(value).range(-100_000.0..=100_000.0).clamp_existing_to_range(false)
                    .prefix(axis).suffix(" px").max_decimals(1))
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
        for sweep in [358.0, 360.0, 362.0, -360.0, 720.0, -1080.0] {
            set_field(&mut app, "Sweep", sweep);
            let radial = &app.array_panel.settings.as_ref().unwrap().radial;
            assert_eq!(radial.sweep, sweep as f32);
            assert!(!radial.closed_loop);
            let last = app.s.selected_elements().last().unwrap().1.transform.translation();
            let radius = (last.x - radial.centre[0]).hypot(last.y - radial.centre[1]);
            assert!((radius - radial.radius - 15.0 * radial.sweep.abs() / 360.0).abs() < 0.001);
        }
        toggle_loop(&mut app);
        assert!(!shows_offset(&mut app));
        let radial = &app.array_panel.settings.as_ref().unwrap().radial;
        assert!(radial.closed_loop);
        assert_eq!(radial.sweep, -360.0);
        toggle_loop(&mut app);
        assert_eq!(app.array_panel.settings.as_ref().unwrap().radial.spiral_offset, 15.0);
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

        let fields = nodes(&ctx, &mut app);
        let count_label = fields.iter().find(|(_, n)| n.value() == Some("Instances")).unwrap().0;
        let count = fields.iter().find(|(_, n)| n.role() == Role::SpinButton && n.labelled_by().contains(&count_label)).unwrap().0;
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
        assert!(app.array_panel.shows_registrations() && app.array_panel.gizmo_endpoints().is_none());
        assert_eq!(app.s.selected_elements().len(), 10);
        click(&ctx, &mut app, &fields.iter().find(|(_, n)| n.label() == Some("Hide Array Handles")).unwrap().1);
        assert!(!app.array_panel.shows_registrations());
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
