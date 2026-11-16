//! One window under the app's existing main-thread event loop.
//! Runtime, not this window, owns and joins the worker that borrows NodeStore.
mod window;

use erlkonig_core::{runtime::Runtime, faculties_browser::ViewerWorker};
use egui::ViewportId;
use egui_wgpu::{winit::Painter, RendererOptions, WgpuConfiguration, WgpuSetup};
use std::num::NonZeroU32;
use std::sync::{Arc, Mutex, atomic::{AtomicBool, Ordering}};
use std::time::Instant;
use winit::{event::WindowEvent, event_loop::ActiveEventLoop, window::{Window, WindowId}};

pub(crate) struct BrowserWindow {
    ui: Option<BrowserUi>,
    worker: Option<ViewerWorker>,
    stop: Arc<AtomicBool>,
}

struct BrowserUi {
    window: Arc<Window>,
    state: egui_winit::State,
    painter: Painter,
    notebook: GORBIE::Notebook,
    repaint: Arc<Mutex<Option<Instant>>>,
}

impl BrowserWindow {
    pub(crate) fn open(event_loop: &ActiveEventLoop, runtime: &mut Runtime) -> Result<Self, String> {
        let attributes = Window::default_attributes()
            .with_title("Erlkonig · Faculties")
            .with_inner_size(winit::dpi::LogicalSize::new(1200.0, 800.0))
            .with_min_inner_size(winit::dpi::LogicalSize::new(768.0, 360.0));
        #[cfg(target_os = "linux")]
        let attributes = {
            use winit::platform::wayland::WindowAttributesExtWayland;
            use winit::platform::x11::WindowAttributesExtX11;
            let attributes = WindowAttributesExtWayland::with_name(attributes, "ai.erlkonig.app", "");
            WindowAttributesExtX11::with_name(attributes, "ai.erlkonig.app", "Erlkonig")
        };
        let window = Arc::new(event_loop.create_window(attributes)
            .map_err(|error| format!("Cannot create the Faculties window: {error}"))?);
        let context = egui::Context::default();
        let mut configuration = WgpuConfiguration::default();
        if let WgpuSetup::CreateNew(setup) = &mut configuration.wgpu_setup {
            setup.power_preference = egui_wgpu::wgpu::PowerPreference::LowPower;
            setup.display_handle = Some(Box::new(event_loop.owned_display_handle()));
        }
        let mut painter = pollster::block_on(Painter::new(
            context.clone(), configuration, false, RendererOptions::default(),
        ));
        pollster::block_on(painter.set_window(ViewportId::ROOT, Some(window.clone())))
            .map_err(|error| format!("Cannot draw the Faculties window: {error}"))?;
        let repaint = Arc::new(Mutex::new(None::<Instant>));
        let scheduled = repaint.clone();
        let wake = Arc::downgrade(&window);
        context.set_request_repaint_callback(move |request| {
            let Some(deadline) = Instant::now().checked_add(request.delay) else { return; };
            let mut pending = scheduled.lock().unwrap_or_else(|error| error.into_inner());
            *pending = Some(pending.map_or(deadline, |old| old.min(deadline)));
            drop(pending);
            if request.delay.is_zero() {
                if let Some(window) = wake.upgrade() { window.request_redraw(); }
            }
            // Delayed background requests are also noticed by the app's bounded
            // status tick; requests during paint set the next loop deadline.
        });
        let stop = Arc::new(AtomicBool::new(false));
        let (notebook, worker) = window::notebook(runtime, &context, stop.clone());
        let state = egui_winit::State::new(context, ViewportId::ROOT, &window,
            Some(window.scale_factor() as f32), window.theme(), painter.max_texture_side());
        window.focus_window();
        window.request_redraw();
        Ok(Self { ui: Some(BrowserUi { window, state, painter, notebook, repaint }), worker, stop })
    }

    pub(crate) fn stop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(worker) = &self.worker { worker.stop(); }
        // Release cell state and frozen observations immediately. Do not join
        // on the UI thread; Runtime retains the worker until it settles.
        self.ui = None;
    }

    pub(crate) fn finished(&self) -> bool {
        self.ui.is_none() && self.worker.as_ref().is_none_or(ViewerWorker::is_finished)
    }

    pub(crate) fn next_wake(&self, normal: Instant) -> Instant {
        let Some(ui) = &self.ui else { return normal; };
        let pending = *ui.repaint.lock().unwrap_or_else(|error| error.into_inner());
        if pending.is_some_and(|deadline| deadline <= Instant::now()) { ui.window.request_redraw(); }
        pending.map_or(normal, |deadline| normal.min(deadline))
    }

    pub(crate) fn on_event(&mut self, id: WindowId, event: &WindowEvent) -> bool {
        let Some(ui) = &mut self.ui else { return false; };
        if ui.window.id() != id { return false; }
        let close = match event {
            WindowEvent::CloseRequested => true,
            WindowEvent::Resized(size) => {
                if let (Some(width), Some(height)) = (NonZeroU32::new(size.width), NonZeroU32::new(size.height)) {
                    ui.painter.on_window_resized(ViewportId::ROOT, width, height);
                }
                false
            }
            WindowEvent::RedrawRequested => ui.paint(),
            _ => false,
        };
        if !close && ui.state.on_window_event(&ui.window, event).repaint { ui.window.request_redraw(); }
        if close { self.stop(); }
        true
    }
}

impl Drop for BrowserWindow { fn drop(&mut self) { self.stop(); } }

impl BrowserUi {
    fn paint(&mut self) -> bool {
        self.repaint.lock().unwrap_or_else(|error| error.into_inner()).take();
        let input = self.state.take_egui_input(&self.window);
        let context = self.state.egui_ctx().clone();
        let format = self.painter.render_state().map(|state| state.target_format);
        let output = context.run_ui(input, |ui| self.notebook.show(ui, format));
        self.state.handle_platform_output(&self.window, output.platform_output);
        let primitives = context.tessellate(output.shapes, output.pixels_per_point);
        self.painter.paint_and_update_textures(ViewportId::ROOT, output.pixels_per_point,
            egui::Rgba::from(context.global_style().visuals.panel_fill).to_array(),
            &primitives, &output.textures_delta, Vec::new());
        output.viewport_output.get(&ViewportId::ROOT).is_some_and(|viewport| {
            viewport.commands.iter().any(|command| matches!(command, egui::ViewportCommand::Close))
        })
    }
}
