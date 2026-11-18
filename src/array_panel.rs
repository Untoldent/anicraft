//! Array panel: stamped (non-procedural) Linear and Radial symbol-instance arrays.
//! Both panels use the same gradient stop bar in colour_panel; beneath it this
//! panel edits properties rather than colours. The ongoing preview is a gesture
//! rolled back and replayed from the original selection on every edit.

use crate::app::App;
use crate::theme::{Section, Type, section, text, with_type};
use anicraft_engine::doc::{ElementId, ElementKind, GradientStop, Rgba};
use egui::{Id, Ui};
use serde_json::{Value, json};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Mode {
    #[default]
    Linear,
    Radial,
}

#[derive(Clone, Debug)]
pub struct State {
    mode: Mode,
    count: u32,
    dx: f32,
    dy: f32,
    start_x: f32,
    start_y: f32,
    center_x: f32,
    center_y: f32,
    radius: f32,
    start_angle: f32,
    sweep_angle: f32,
    rotate: bool,
    backwards: bool,
    stops: Vec<GradientStop>,
    /// The original instance selected when live preview began.
    source: Option<ElementId>,
    /// The original instance whose coordinates the controls show.
    loaded: Option<ElementId>,
    previewing: bool,
    error: Option<String>,
}

impl Default for State {
    fn default() -> Self {
        let stop = |offset, alpha| GradientStop {
            offset,
            color: Rgba::new(0.25, 0.5, 0.95, alpha),
        };
        Self {
            mode: Mode::Linear, count: 10,
            dx: 36.0, dy: 0.0,
            start_x: 0.0, start_y: 0.0,
            center_x: 0.0, center_y: 0.0, radius: 120.0,
            start_angle: 0.0, sweep_angle: 360.0,
            rotate: false, backwards: false,
            stops: vec![stop(0.0, 1.0), stop(1.0, 1.0)],
            source: None, loaded: None, previewing: false, error: None,
        }
    }
}

impl State {
    fn params(&self) -> Value {
        let alpha_stops: Vec<Value> = self.stops.iter().map(|s| json!({
            "offset": s.offset, "alpha": s.color.a,
        })).collect();
        let mut p = match self.mode {
            Mode::Linear => json!({
                "mode": "linear", "count": self.count,
                "start_x": self.start_x, "start_y": self.start_y,
                "dx": self.dx, "dy": self.dy,
            }),
            Mode::Radial => json!({
                "mode": "radial", "count": self.count,
                "center_x": self.center_x, "center_y": self.center_y,
                "radius": self.radius,
                "start_angle": self.start_angle,
                "sweep_angle": self.sweep_angle, "rotate": self.rotate,
            }),
        };
        if let Some(map) = p.as_object_mut() {
            map.insert("stacking".into(), json!(if self.backwards { "front_to_back" } else { "back_to_front" }));
            map.insert("alpha_stops".into(), json!(alpha_stops));
        }
        p
    }

    fn refresh(&mut self, app: &mut App) {
        if !self.previewing {
            app.s.begin_gesture(tr_noop!("Array"));
            self.previewing = true;
        }
        app.s.revert_gesture();
        match app.s.execute("array.create", &self.params()) {
            Ok(_) => self.error = None,
            Err(e) => self.error = Some(e.to_string()),
        }
    }

    fn cancel(&mut self, app: &mut App) {
        if self.previewing {
            app.s.revert_gesture();
            app.s.end_gesture();
        }
        self.previewing = false;
        self.source = None;
        self.error = None;
    }

    fn commit(&mut self, app: &mut App) {
        if self.previewing && self.error.is_none() {
            app.s.end_gesture();
            self.previewing = false;
            self.source = None;
        }
    }
}

/// Called by the dock. The document is not changed until preview starts; after that,
/// adjusting geometry or the stop ramp updates the stage without accumulating copies.
pub fn panel(app: &mut App, ui: &mut Ui) {
    // Hold state locally, so stage commands can borrow the app during the UI event.
    let mut state = std::mem::take(&mut app.array_panel);
    let selection = app.s.selected_elements();
    let single = if let [(_, e)] = selection.as_slice() {
        if matches!(&e.kind, ElementKind::Symbol { .. }) {
            Some((e.id, e.transform.m[4], e.transform.m[5]))
        } else { None }
    } else { None };

    if !state.previewing
        && let Some((id, x, y)) = single
        && state.loaded != Some(id)
    {
        state.loaded = Some(id);
        state.start_x = x;
        state.start_y = y;
        state.center_x = x - state.radius;
        state.center_y = y;
    }
    let usable = state.previewing || single.is_some();
    if !usable {
        ui.label(text(Type::Caption, tr!("Select one symbol instance to make an array.")));
    }

    let mut changed = false;
    ui.add_enabled_ui(usable, |ui| {
        section(ui, "array_distribution", Section::Rule, tr!("Distribution"), None, |ui| {
            ui.horizontal(|ui| {
                ui.selectable_value(&mut state.mode, Mode::Linear, tr!("Linear"));
                ui.selectable_value(&mut state.mode, Mode::Radial, tr!("Radial"));
            });
            // A mode switch changes the array even when no numeric value did.
            // These selectable buttons are handled by the mode comparison below.
            ui.horizontal(|ui| {
                ui.label(tr!("Instances"));
                changed |= with_type(ui, Type::Data, |ui| ui.add(egui::DragValue::new(&mut state.count).range(1..=500).speed(1))).changed();
            });
            match state.mode {
                Mode::Linear => {
                    changed |= field(ui, tr!("Start X"), &mut state.start_x, " px");
                    changed |= field(ui, tr!("Start Y"), &mut state.start_y, " px");
                    changed |= field(ui, tr!("Spacing X"), &mut state.dx, " px");
                    changed |= field(ui, tr!("Spacing Y"), &mut state.dy, " px");
                }
                Mode::Radial => {
                    changed |= field(ui, tr!("Centre X"), &mut state.center_x, " px");
                    changed |= field(ui, tr!("Centre Y"), &mut state.center_y, " px");
                    changed |= field(ui, tr!("Radius"), &mut state.radius, " px");
                    state.radius = state.radius.max(0.0);
                    changed |= field(ui, tr!("Start angle"), &mut state.start_angle, " °");
                    changed |= field(ui, tr!("Sweep"), &mut state.sweep_angle, " °");
                    changed |= crate::theme::checkbox(ui, &mut state.rotate, tr!("Rotate instances")).changed();
                }
            }
            changed |= crate::theme::checkbox(ui, &mut state.backwards, tr!("Stack first copy in front")).changed();
        });
        ui.add_space(4.0);
        section(ui, "array_stops", Section::Rule, tr!("Property stops"), None, |ui| {
            ui.label(text(Type::Caption, tr!("Drag stops to control how properties change across the array.")));
            if crate::colour_panel::array_stop_bar(ui, Id::new("array_stop_bar"), &mut state.stops).is_some() {
                changed = true;
            }
        });
    });

    if let Some(error) = &state.error {
        ui.label(text(Type::Caption, error));
    }
    ui.add_space(8.0);
    let mut preview = false;
    let mut commit = false;
    let mut cancel = false;
    ui.horizontal(|ui| {
        if state.previewing {
            commit = ui.add_enabled(state.error.is_none(), egui::Button::new(tr!("Commit"))).clicked();
            cancel = ui.button(tr!("Cancel")).clicked();
        } else {
            preview = ui.add_enabled(usable, egui::Button::new(tr!("Preview"))).clicked();
        }
    });

    // Check the mode *after* controls render: the buttons above modify it directly.
    // Keep a persistent prior mode without relying on generated instances' selection.
    let mode_key = Id::new("array_previous_mode");
    let old_mode = ui.data(|d| d.get_temp::<Mode>(mode_key)).unwrap_or(state.mode);
    if old_mode != state.mode { changed = true; }
    ui.data_mut(|d| d.insert_temp(mode_key, state.mode));

    if cancel {
        state.cancel(app);
    } else if commit {
        state.commit(app);
    } else if preview || (changed && usable) {
        // Remember the source *before* running a preview, while it is still selected.
        if !state.previewing { state.source = single.map(|s| s.0); }
        state.refresh(app);
    }
    app.array_panel = state;
}

fn field(ui: &mut Ui, label: &str, value: &mut f32, suffix: &'static str) -> bool {
    let mut changed = false;
    ui.horizontal(|ui| {
        ui.label(label);
        changed = with_type(ui, Type::Data, |ui| {
            ui.add(egui::DragValue::new(value).range(-100_000.0..=100_000.0).suffix(suffix).speed(1.0).max_decimals(1))
        }).changed();
    });
    changed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stops_are_sent_as_numeric_alpha_not_as_colour() {
        let mut s = State::default();
        s.mode = Mode::Radial;
        s.stops[0].color.a = 0.75;
        s.stops.push(GradientStop { offset: 0.4, color: Rgba::new(0.1, 0.3, 0.8, 1.0) });
        let p = s.params();
        assert_eq!(p["alpha_stops"][0]["alpha"], 0.75);
        assert_eq!(p["alpha_stops"][2]["offset"], 0.4);
        assert_eq!(p["mode"], "radial");
    }
}
