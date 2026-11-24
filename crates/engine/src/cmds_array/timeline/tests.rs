use super::*;
use crate::format;
use anicraft_doc::{ColorTransform, ElementId, Layer, MotionTween, evaluate_layer_elements};

fn source() -> Session {
    let mut s = Session::default();
    s.execute("draw.rect", &json!({"x": 32, "y": 40, "w": 24, "h": 14})).unwrap();
    s.execute("modify.convert_to_symbol", &json!({"name": "Source"})).unwrap();
    s
}

fn layer(s: &Session) -> &Layer {
    s.timeline().layer(s.layer).unwrap()
}

fn at(s: &Session, frame: u32) -> Arc<Element> {
    let (_, poses) = evaluate_layer_elements(layer(s), frame).unwrap();
    assert_eq!(poses.len(), 1);
    poses.first().unwrap().clone()
}

fn near(a: f32, b: f32) {
    assert!((a - b).abs() < 0.001, "{a} != {b}");
}

fn compare_pose(a: &Element, b: &Element) {
    for (a, b) in a.transform.m.into_iter().zip(b.transform.m) {
        near(a, b);
    }
    let (a_colour, b_colour) = (a.color.transform(), b.color.transform());
    for (a, b) in a_colour.mul.into_iter().chain(a_colour.add).zip(b_colour.mul.into_iter().chain(b_colour.add)) {
        near(a, b);
    }
    let a_filters = a.kind.filters().unwrap();
    let b_filters = b.kind.filters().unwrap();
    assert_eq!(a_filters.len(), b_filters.len());
    for (a, b) in a_filters.iter().zip(b_filters) {
        assert_eq!(a.enabled, b.enabled);
        assert_eq!(a.filter.label(), b.filter.label());
        for param in a.filter.params() {
            near(a.filter.param(param).unwrap(), b.filter.param(param).unwrap());
        }
    }
}

#[test]
fn classic_array_timeline_matches_spatial_source_values_and_independent_eases() {
    let mut s = source();
    s.execute("symbol.set_kind", &json!({"kind": "movie_clip"})).unwrap();
    s.execute(
        "element.set_filters",
        &json!({"filters": [
            {"type": "blur", "blur_x": 2, "blur_y": 4, "quality": 2},
            {"type": "adjust_color", "hue": 35, "enabled": false},
        ]}),
    )
    .unwrap();
    let id = s.selection.first().unwrap();
    let original = s.doc.clone();
    let steps = s.history_len();
    let stops: Vec<_> = [(1.0, 0.0, 1.0, "QuadIn", 2), (2.0, 48.0, 0.5, "QuadOut", 8), (0.8, -12.0, 0.25, "Linear", 5)]
        .into_iter()
        .enumerate()
        .map(|(i, (scale, rotation, alpha, ease, blur))| {
            json!({
                "offset": i as f32 / 2.0, "scale_x": scale, "scale_y": 1.6 - i as f32 * 0.3,
                "rotation": rotation, "alpha": alpha, "outbound_ease": ease,
                "color_effect": ColorEffect::Advanced(ColorTransform { add: [i as f32 * 0.2, 0.0, 0.0, 0.0], ..ColorTransform::IDENTITY }),
                "filters": [{"type": "blur", "blur_x": blur, "blur_y": 4},
                            {"type": "adjust_color", "hue": 150}],
            })
        })
        .collect();
    let mut p = json!({"start_x": 360, "start_y": 20, "dx": -5, "dy": 2,
        "end_x": 120, "end_y": 116, "duration": 49, "count": 49, "source_values": true,
        "property_stops": stops,
        "easing": {"mode": "per_segment", "colour_effect": false}});
    s.preview_array(&p).unwrap();
    let spatial: Vec<_> = s.selected_elements().into_iter().map(|(_, pose)| pose).collect();
    p["count"] = json!(2); // A Classic span depends only on duration, geometry, and stops.
    s.commit_array_timeline_preview(&p).unwrap();
    assert!(!s.array_preview_active());
    assert_eq!(s.history_len(), steps + 1);
    assert_eq!(s.selection.first(), Some(id));
    assert_eq!(s.doc.library.len(), original.library.len());
    assert_eq!(layer(&s).keyframes.iter().map(|k| (k.start, k.duration)).collect::<Vec<_>>(), [(0, 24), (24, 24), (48, 1)]);
    assert_eq!(
        layer(&s).keyframes.first().unwrap().tween.as_ref().unwrap().groups,
        [
            (EaseGroup::Position, Easing::QuadIn),
            (EaseGroup::Rotation, Easing::QuadIn),
            (EaseGroup::Scale, Easing::QuadIn),
            (EaseGroup::Filters, Easing::QuadIn)
        ]
    );
    for (frame, pose) in (0..49).zip(&spatial) {
        let animated = at(&s, frame);
        assert_eq!(animated.id, id.1);
        compare_pose(&animated, pose);
        assert!(matches!(animated.kind.filters().unwrap().last().unwrap().filter, anicraft_doc::Filter::AdjustColor { hue: 35.0, .. }));
    }
    assert!(evaluate_layer_elements(layer(&s), 49).is_none());

    let animated = s.doc.clone();
    let saved = format::to_json(&s.doc).unwrap();
    let mut reopened = Session::new(format::from_json(&saved).unwrap());
    assert_eq!(layer(&reopened).keyframes, layer(&s).keyframes);
    reopened.execute("timeline.create_tween", &json!({"easing": "linear"})).unwrap();
    assert!(layer(&reopened).keyframes.first().unwrap().tween.is_some());
    s.undo().unwrap();
    assert!(s.doc == original);
    s.redo().unwrap();
    assert!(s.doc == animated);
}

#[test]
fn holds_match_eased_spatial_samples_and_preserve_order_at_a_later_playhead() {
    let mut s = source();
    s.frame = 5;
    let source = s.layer_mut(None).unwrap().keyframe_mut(0).unwrap();
    source.start = 5;
    let id = s.selection.first().unwrap().1;
    let p = json!({"animation": "hold", "duration": 48, "count": 8, "start_x": 100,
        "start_y": 100, "end_x": 30, "end_y": 100, "dx": -10, "dy": 0, "source_values": true,
        "easing": {"preset": "BackOut"},
        "property_stops": [{"offset": 0, "alpha": 1}, {"offset": 0.35, "alpha": 0.2}, {"offset": 1, "alpha": 0.8}]});
    s.preview_array(&p).unwrap();
    let preview: Vec<_> = s.selected_elements().into_iter().map(|(_, pose)| pose).collect();
    s.commit_array_timeline_preview(&p).unwrap();
    assert_eq!(layer(&s).keyframes.iter().map(|k| k.start).collect::<Vec<_>>(), [5, 12, 18, 25, 32, 39, 45, 52]);
    for (key, spatial) in layer(&s).keyframes.iter().zip(&preview) {
        assert!(key.tween.is_none() && key.motion.is_none());
        let pose = at(&s, key.end() - 1);
        assert_eq!(pose.id, id);
        compare_pose(&pose, spatial);
    }
    assert!(evaluate_layer_elements(layer(&s), 4).is_none());
    assert!(evaluate_layer_elements(layer(&s), 53).is_none());
}

#[test]
fn validation_keeps_the_live_preview_and_rejects_lossy_classic_conversions() {
    let mut s = source();
    let p = json!({"duration": 48, "count": 8, "start_x": 20, "end_x": 120, "dx": 100.0 / 7.0,
        "source_values": true, "property_stops": [{"offset": 0}, {"offset": 0.45, "alpha": 0.4}, {"offset": 1}]});
    s.preview_array(&p).unwrap();
    let preview = s.doc.clone();
    let selection = s.selection.clone();
    let steps = s.history_len();
    for (key, value, message) in [
        ("duration", json!(2), "same frame"),
        ("duration", json!(MAX_FRAME + 2), "Duration must be"),
        ("easing", json!({"preset": "QuadIn"}), "Global easing"),
        ("property_stops", json!([{"offset": 0}, {"offset": 1, "rotation": 360}]), "shortest rotation"),
        ("property_stops", json!([{"offset": 0}, {"offset": 0}]), "identical positions"),
        ("mode", json!("radial"), "Linear Arrays only"),
    ] {
        let mut invalid = p.clone();
        invalid[key] = value;
        let error = s.commit_array_timeline_preview(&invalid).unwrap_err().to_string();
        assert!(error.contains(message), "{error}");
        assert!(s.doc == preview && s.selection == selection && s.history_len() == steps);
        assert!(s.array_preview_active());
    }
    let mut short_hold = p.clone();
    short_hold["animation"] = json!("hold");
    short_hold["duration"] = json!(7);
    assert!(s.commit_array_timeline_preview(&short_hold).unwrap_err().to_string().contains("8 frames"));
    assert!(s.doc == preview);

    s.commit_array_timeline_preview(&p).unwrap();
    assert_eq!(layer(&s).keyframes.iter().map(|k| k.start).collect::<Vec<_>>(), [0, 21, 47]);
}

#[test]
fn classic_spans_can_share_a_layer_with_nonoverlapping_motion() {
    let mut s = source();
    let mut unrelated = (*at(&s, 0)).clone();
    unrelated.id = ElementId(s.doc.alloc_id());
    let mut motion_key = Keyframe::new(24, 10);
    motion_key.motion = Some(MotionTween::new(unrelated.id));
    motion_key.elements = vec![Arc::new(unrelated)];
    let motion_key = Arc::new(motion_key);
    let source_layer = s.layer_mut(None).unwrap();
    source_layer.keyframes.push(Arc::new(Keyframe::new(4, 20)));
    source_layer.keyframes.push(motion_key.clone());
    let original = s.doc.clone();
    let params = json!({"end_x": 120, "duration": 30});
    let error = s.execute("array.create_timeline", &params).unwrap_err().to_string();
    assert!(error.contains("frame 25"), "{error}");
    assert!(s.doc == original);
    s.execute("array.create_timeline", &json!({"end_x": 120, "duration": 14})).unwrap();
    assert!(layer(&s).keyframes.first().unwrap().tween.is_some());
    assert_eq!(layer(&s).keyframes.get(2).map(|key| (key.start, key.end(), key.elements.is_empty())), Some((14, 24, true)));
    assert!(Arc::ptr_eq(layer(&s).keyframes.last().unwrap(), &motion_key));
}

#[test]
fn classic_global_easing_can_drive_a_two_stop_span() {
    let mut s = source();
    let p = json!({"start_x": 20, "end_x": 100, "duration": 5,
        "easing": {"preset": "QuadIn", "transform": false},
        "property_stops": [{"offset": 0, "rotation": -60, "alpha": 1},
                           {"offset": 1, "rotation": 50, "alpha": 0.2}]});
    s.execute("array.create_timeline", &p).unwrap();
    let middle = at(&s, 2);
    near(middle.transform.m[4], 40.0);
    near(middle.transform.decompose().rotation.to_degrees(), -5.0);
    near(middle.color.alpha(), 0.8);
}

#[test]
fn duration_reports_all_groups_and_finds_the_first_valid_duration() {
    let stops: Vec<_> = [0.0, 0.12, 0.13, 0.7, 0.71, 1.0].into_iter().map(|t| ArrayPropertyStop::new(t, 1.0)).collect();
    let times = classic_times(&stops, &ArrayEasing::default(), true).unwrap();
    let error = frame_offsets(&times, &stops, true, 20, 10).unwrap_err();
    let EngineError::ArrayDuration { message, suggested } = error else { panic!("duration collision expected") };
    assert!(message.contains("Frame 22: Stop #2 (12%), Stop #3 (13%)"), "{message}");
    assert!(message.contains("Frame 27: Stop #4 (70%), Stop #5 (71%)"), "{message}");
    assert_eq!(suggested, Some(13));
    assert!(frame_offsets(&times, &stops, true, 20, 13).is_ok());
    assert!(frame_offsets(&times, &stops, true, 20, 14).is_err());
    assert!(matches!(frame_offsets(&times, &stops, true, MAX_FRAME - 9, 10),
        Err(EngineError::ArrayDuration { suggested: None, .. })));
}

#[test]
fn duration_reports_synthetic_endpoints_coincident_stops_and_hold_samples() {
    let stops = [ArrayPropertyStop::new(0.03, 1.0), ArrayPropertyStop::new(0.03, 1.0)];
    let times = classic_times(&stops, &ArrayEasing::default(), false).unwrap();
    let error = frame_offsets(&times, &stops, true, 0, 12).unwrap_err();
    let EngineError::ArrayDuration { message, suggested: None } = error else { panic!("coincident stops expected") };
    assert!(message.contains("Start endpoint (0%), Stop #1 (3%), Stop #2 (3%)"), "{message}");
    assert!(message.contains("Increasing the duration cannot separate them"));
    let hold = frame_offsets(&[0.0, 1.0 / 3.0, 2.0 / 3.0, 1.0], &[], false, 4, 2).unwrap_err();
    assert!(hold.to_string().contains("Frame 5: Sample #1 (0%), Sample #2"), "{hold}");
    assert!(matches!(hold, EngineError::ArrayDuration { suggested: Some(4), .. }));
    assert_eq!(array_timeline_default_duration(23.976, 0), 24);
    assert_eq!(array_timeline_default_duration(60.0, MAX_FRAME - 6), 7);
}
