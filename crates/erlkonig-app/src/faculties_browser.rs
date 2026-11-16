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

fn schedule_repaints(context: &egui::Context, wake: Box<dyn Fn() + Send + Sync>) -> Arc<Mutex<Option<Instant>>> {
    let repaint = Arc::new(Mutex::new(None::<Instant>));
    let scheduled = repaint.clone();
    context.set_request_repaint_callback(move |request| {
        let Some(deadline) = Instant::now().checked_add(request.delay) else { return; };
        let mut pending = scheduled.lock().unwrap_or_else(|error| error.into_inner());
        if pending.is_some_and(|old| old <= deadline) { return; }
        *pending = Some(deadline);
        drop(pending);
        // egui calls this with its context locked. Window::request_redraw can
        // synchronously dispatch to the Mac main thread, which needs that lock.
        // Queue only; next_wake requests the actual redraw on the event loop.
        wake();
    });
    repaint
}

impl BrowserWindow {
    pub(crate) fn open(event_loop: &ActiveEventLoop, runtime: &mut Runtime, wake: Box<dyn Fn() + Send + Sync>) -> Result<Self, String> {
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
        let repaint = schedule_repaints(&context, wake);
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
        // A completed redraw is not fresh input. egui-winit marks it as
        // repainting, which would otherwise schedule another frame forever.
        if !close && !matches!(event, WindowEvent::RedrawRequested)
            && ui.state.on_window_event(&ui.window, event).repaint {
            ui.window.request_redraw();
        }
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

#[cfg(test)]
mod repaint_tests {
    use super::*;
    use std::sync::mpsc;
    use std::time::Duration;

    #[test]
    fn background_repaint_returns_before_the_main_thread_consumes_its_wake() {
        let context = egui::Context::default();
        let (wake_tx, wake_rx) = mpsc::channel();
        let pending = schedule_repaints(&context, Box::new(move || { wake_tx.send(()).unwrap(); }));
        let (done_tx, done_rx) = mpsc::channel();
        let worker_context = context.clone();
        let worker = std::thread::spawn(move || {
            worker_context.request_repaint();
            done_tx.send(()).unwrap();
        });
        // Do not service the event-loop queue until the worker has released
        // egui's lock. A synchronous main-thread dispatch cannot satisfy this.
        done_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        worker.join().unwrap();
        let _ = context.style(); // The main thread can acquire egui again.
        assert!(pending.lock().unwrap().is_some_and(|at| at <= Instant::now()));
        assert_eq!(wake_rx.try_iter().count(), 1);
    }

    #[test]
    fn delayed_repaints_wake_the_loop_and_keep_the_earliest_deadline() {
        let context = egui::Context::default();
        let (wake_tx, wake_rx) = mpsc::channel();
        let pending = schedule_repaints(&context, Box::new(move || { wake_tx.send(()).unwrap(); }));
        context.request_repaint_after(Duration::from_secs(10));
        let first = pending.lock().unwrap().unwrap();
        assert!(first > Instant::now());
        context.request_repaint_after(Duration::from_secs(20));
        assert_eq!(pending.lock().unwrap().unwrap(), first);
        assert_eq!(wake_rx.try_iter().count(), 1);
        context.request_repaint_after(Duration::from_secs(2));
        assert!(pending.lock().unwrap().unwrap() < first);
        context.request_repaint();
        context.request_repaint();
        assert!(pending.lock().unwrap().is_some_and(|at| at <= Instant::now()));
        assert_eq!(wake_rx.try_iter().count(), 2);
    }
}
