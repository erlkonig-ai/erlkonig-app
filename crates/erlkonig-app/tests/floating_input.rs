//! Exercise the viewer's real notebook/float/graph helpers under the app lock.
//! No native window, device, store, network or synthetic input dispatcher.

use std::{cell::RefCell, rc::Rc};

use egui::{vec2, Event, Modifiers, PointerButton, Pos2, Rect, Sense, Vec2};
use GORBIE::{graph::GraphViewport, Notebook, NotebookConfig};

struct Scene {
    viewport: GraphViewport,
    graph: Rect,
    floats: Vec<(Rect, egui::Order)>,
    float_count: usize,
    closed: bool,
}

struct Fixture {
    context: egui::Context,
    notebook: Notebook,
    scene: Rc<RefCell<Scene>>,
    time: f64,
}

impl Fixture {
    fn new(float_count: usize) -> Self {
        let context = egui::Context::default();
        let scene = Rc::new(RefCell::new(Scene {
            viewport: GraphViewport::default(),
            graph: Rect::NOTHING,
            floats: vec![(Rect::NOTHING, egui::Order::Foreground); float_count],
            float_count: 0,
            closed: false,
        }));
        let shared = scene.clone();
        let notebook = NotebookConfig::new("floating input fixture")
            .without_editor()
            .embedded(&context, move |nb| {
                let shared = shared.clone();
                nb.view(move |ctx| {
                    let mut scene = shared.borrow_mut();
                    let width = ctx.available_width();
                    let (rect, response) = ctx.ui_mut()
                        .allocate_exact_size(vec2(width, 640.0), Sense::click_and_drag());
                    scene.graph = rect;
                    scene.viewport.interact(ctx.ui_mut(), &response, rect);
                    for index in 0..scene.float_count {
                        let response = ctx.push_id(index, |ctx| ctx.float(|ctx| {
                            let width = ctx.available_width();
                            let order = ctx.layer_id().order;
                            let (rect, _) = ctx.ui_mut()
                                .allocate_exact_size(vec2(width, 180.0), Sense::hover());
                            scene.floats[index] = (rect, order);
                        })).inner;
                        scene.closed |= response.closed;
                    }
                });
            });
        let mut fixture = Self { context, notebook, scene, time: 0.0 };
        fixture.frame(vec![], Modifiers::NONE);
        fixture.frame(vec![], Modifiers::NONE);
        let origin = fixture.scene.borrow().graph.min + vec2(100.0, 100.0);
        fixture.scene.borrow_mut().float_count = float_count;
        fixture.frame(vec![Event::PointerMoved(origin)], Modifiers::NONE);
        // Areas have an initial sizing pass; do not test first-frame geometry.
        for _ in 0..3 {
            fixture.frame(vec![], Modifiers::NONE);
        }
        fixture
    }

    fn frame(&mut self, events: Vec<Event>, modifiers: Modifiers) {
        self.time += 1.0 / 60.0;
        let _ = self.context.run_ui(egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(1000.0, 900.0))),
            time: Some(self.time),
            events,
            modifiers,
            ..Default::default()
        }, |ui| self.notebook.show(ui, None));
    }

    fn handle(&self, index: usize) -> Pos2 {
        let scene = self.scene.borrow();
        let at = scene.floats[index].0.min
            + vec2(24.0, 1.0 + GORBIE::card_ctx::GRID_ROW_MODULE / 2.0);
        assert!(scene.graph.contains(at), "handle must actually overlap graph: {at:?}");
        at
    }

    fn drag(&mut self, from: Pos2, delta: Vec2) {
        self.frame(vec![Event::PointerMoved(from)], Modifiers::NONE);
        self.frame(vec![Event::PointerButton {
            pos: from, button: PointerButton::Primary, pressed: true,
            modifiers: Modifiers::NONE,
        }], Modifiers::NONE);
        for fraction in [0.5, 1.0] {
            self.frame(vec![Event::PointerMoved(from + delta * fraction)], Modifiers::NONE);
        }
        self.frame(vec![Event::PointerButton {
            pos: from + delta, button: PointerButton::Primary, pressed: false,
            modifiers: Modifiers::NONE,
        }], Modifiers::NONE);
        self.frame(vec![], Modifiers::NONE);
    }

    fn graph_origin(&self) -> Pos2 {
        let scene = self.scene.borrow();
        scene.viewport.to_screen(scene.graph, [0.0, 0.0])
    }

    fn assert_graph_untouched(&self, before: Pos2) {
        let scene = self.scene.borrow();
        assert_eq!(self.graph_origin(), before, "underlying graph panned");
        assert_eq!(scene.viewport.zoom(), 1.0, "underlying graph zoomed");
        assert!(!scene.viewport.touched(), "underlying graph claimed gesture");
        assert!(!scene.closed, "drag must not close the float");
    }
}

#[test]
fn content_float_handle_moves_without_panning_graph() {
    let mut fixture = Fixture::new(1);
    let from = fixture.handle(0);
    let float_before = fixture.scene.borrow().floats[0].0.min;
    let graph_before = fixture.graph_origin();
    fixture.drag(from, vec2(40.0, 30.0));
    let float_after = fixture.scene.borrow().floats[0].0.min;
    eprintln!("float {float_before:?} -> {float_after:?}; graph {graph_before:?} -> {:?}", fixture.graph_origin());
    assert!((float_after - float_before).length() > 20.0, "actual handle did not move float");
    fixture.assert_graph_untouched(graph_before);
}

#[test]
fn stacked_float_handle_moves_only_front_float() {
    let mut fixture = Fixture::new(2);
    let from = fixture.handle(1);
    let before = fixture.scene.borrow().floats.clone();
    let graph_before = fixture.graph_origin();
    fixture.drag(from, vec2(40.0, 30.0));
    let after = fixture.scene.borrow().floats.clone();
    eprintln!("stack before {before:?}; after {after:?}");
    assert_eq!(after[0].0.min, before[0].0.min, "back float moved");
    assert!((after[1].0.min - before[1].0.min).length() > 20.0, "front float did not move");
    fixture.assert_graph_untouched(graph_before);
}

#[test]
fn viewport_anchored_float_handle_moves_without_panning_graph() {
    let mut fixture = Fixture::new(1);
    let from = fixture.handle(0);
    let graph_before = fixture.graph_origin();
    // Use the real handle to cross the notebook's right-edge anchor threshold.
    fixture.drag(from, vec2(550.0, 0.0));
    assert_eq!(fixture.scene.borrow().floats[0].1, egui::Order::Tooltip,
        "setup did not reach actual viewport anchor");
    let from = fixture.handle(0);
    let before = fixture.scene.borrow().floats[0].0.min;
    fixture.drag(from, vec2(20.0, 30.0));
    assert!((fixture.scene.borrow().floats[0].0.min - before).length() > 10.0);
    fixture.assert_graph_untouched(graph_before);
}

#[test]
fn floating_layer_blocks_graph_pinch_zoom() {
    let mut fixture = Fixture::new(1);
    let at = fixture.handle(0) + vec2(0.0, 60.0);
    let before = fixture.graph_origin();
    fixture.frame(vec![Event::PointerMoved(at), Event::Zoom(1.5)], Modifiers::NONE);
    fixture.assert_graph_untouched(before);
}

#[test]
fn floating_layer_blocks_graph_command_wheel_zoom() {
    let mut fixture = Fixture::new(1);
    let at = fixture.handle(0) + vec2(0.0, 60.0);
    let before = fixture.graph_origin();
    let modifiers = Modifiers { ctrl: true, command: true, ..Modifiers::NONE };
    fixture.frame(vec![Event::PointerMoved(at), Event::MouseWheel {
        unit: egui::MouseWheelUnit::Point, delta: vec2(0.0, 30.0), modifiers,
        phase: egui::TouchPhase::Move,
    }], modifiers);
    fixture.assert_graph_untouched(before);
}

#[test]
fn exposed_graph_still_accepts_drag_pinch_and_command_wheel() {
    let mut fixture = Fixture::new(1);
    let at = fixture.scene.borrow().graph.left_bottom() + vec2(30.0, -40.0);
    assert!(!fixture.scene.borrow().floats[0].0.contains(at));
    let before = fixture.graph_origin();
    fixture.drag(at, vec2(30.0, -20.0));
    assert_ne!(fixture.graph_origin(), before, "control: exposed graph must pan");
    fixture.frame(vec![Event::PointerMoved(at), Event::Zoom(1.5)], Modifiers::NONE);
    let zoom = fixture.scene.borrow().viewport.zoom();
    assert!(zoom > 1.0, "control: exposed graph must accept pinch");
    let modifiers = Modifiers { ctrl: true, command: true, ..Modifiers::NONE };
    fixture.frame(vec![Event::MouseWheel {
        unit: egui::MouseWheelUnit::Point, delta: vec2(0.0, 30.0), modifiers,
        phase: egui::TouchPhase::Move,
    }], modifiers);
    assert_ne!(fixture.scene.borrow().viewport.zoom(), zoom,
        "control: exposed graph must accept command-wheel");
}
