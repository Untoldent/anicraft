//! Explicit normalized arc-length timing, independent of position chords.

use crate::{EaseFamily, Easing};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(super) struct PathTiming {
    // Each key starts an ease from its own progress to the next boundary (or 1).
    segments: Vec<(f32, Easing)>,
    // A split tween retains the remainder of the original curve, without refitting an ease.
    #[serde(default, skip_serializing_if = "is_zero")]
    start: f32,
    #[serde(default, skip_serializing_if = "is_zero")]
    turn: f32,
}

fn is_zero(value: &f32) -> bool {
    *value == 0.0
}

impl PathTiming {
    pub(super) fn new(segments: Vec<(f32, Easing)>, turn: f32) -> Option<Self> {
        let timing = Self { segments, start: 0.0, turn };
        timing.is_sane().then_some(timing)
    }

    pub(super) fn is_sane(&self) -> bool {
        (0.0..1.0).contains(&self.start)
            && (-360.0..=360.0).contains(&self.turn)
            && self.segments.len() <= super::MAX_KEYS
            && self.segments.first().is_some_and(|(t, _)| *t == 0.0)
            && self.segments.windows(2).all(|p| matches!(p, [a, b] if a.0 < b.0))
            && self.segments.iter().all(|(t, ease)| {
                (0.0..1.0).contains(t)
                    && (*ease == Easing::Linear
                        || matches!(
                            ease.family(),
                            Some((EaseFamily::Quad | EaseFamily::Cubic | EaseFamily::Quart | EaseFamily::Quint | EaseFamily::Sine | EaseFamily::Circ, _))
                        ))
            })
    }

    pub(super) fn progress(&self, time: f32) -> f32 {
        let time = self.start + (1.0 - self.start) * time;
        let index = self.segments.partition_point(|(at, _)| *at <= time).saturating_sub(1);
        let Some((a, ease)) = self.segments.get(index) else { return time };
        let b = self.segments.get(index + 1).map_or(1.0, |key| key.0);
        let span = b - a;
        if span <= f32::EPSILON || time == *a || *ease == Easing::Linear {
            return time;
        }
        a + span * ease.apply((time - a) / span)
    }

    pub(super) fn rotation(&self, path_progress: f32) -> f32 {
        self.turn * path_progress
    }

    pub(super) fn tail(&self, time: f32, path_progress: f32) -> Option<Self> {
        let start = self.start + (1.0 - self.start) * time.max(0.0);
        (start < 1.0).then(|| Self { start, turn: self.turn * (1.0 - path_progress), ..self.clone() })
    }
}
