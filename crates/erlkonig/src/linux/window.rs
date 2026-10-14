//! The status window: the tray menu's state and actions, plus the prompts
//! that a native modal alert shows on macOS. egui in a winit window, drawn
//! through wgpu, created only while it is shown.

use crate::app::{Control, View};
use egui::{RichText, ViewportId};
use egui_wgpu::winit::Painter;
use egui_wgpu::{RendererOptions, WgpuConfiguration, WgpuSetup};
use std::num::NonZeroU32;
use std::sync::Arc;
use tray_icon::menu::MenuId;
use winit::dpi::LogicalSize;
use winit::event::WindowEvent;
use winit::event_loop::ActiveEventLoop;
use winit::platform::wayland::WindowAttributesExtWayland;
use winit::platform::x11::WindowAttributesExtX11;
use winit::window::{Window, WindowId};

/// What the person did in the window, in the order they did it.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Action {
    /// The same event as choosing the tray menu item with this id.
    Menu(MenuId),
    /// The oldest prompt was answered with this button.
    Answer(usize),
}

pub(crate) struct StatusWindow {
    window: Arc<Window>,
    state: egui_winit::State,
    painter: Painter,
    view: View,
    actions: Vec<Action>,
}

impl StatusWindow {
    pub(crate) fn open(event_loop: &ActiveEventLoop, view: View) -> Result<Self, String> {
        let attributes = Window::default_attributes()
            .with_title("Erlkonig")
            .with_inner_size(LogicalSize::new(440.0, 520.0))
            .with_min_inner_size(LogicalSize::new(320.0, 240.0));
        // The desktop matches windows to ai.erlkonig.app.desktop by these.
        let attributes = WindowAttributesExtWayland::with_name(attributes, "ai.erlkonig.app", "");
        let attributes =
            WindowAttributesExtX11::with_name(attributes, "ai.erlkonig.app", "Erlkonig");
        let window = Arc::new(
            event_loop
                .create_window(attributes)
                .map_err(|error| format!("cannot create the status window: {error}"))?,
        );
        let context = egui::Context::default();
        let mut configuration = WgpuConfiguration::default();
        if let WgpuSetup::CreateNew(setup) = &mut configuration.wgpu_setup {
            // A status window never needs the discrete GPU.
            setup.power_preference = wgpu::PowerPreference::LowPower;
            setup.display_handle = Some(Box::new(event_loop.owned_display_handle()));
        }
        let mut painter = pollster::block_on(Painter::new(
            context.clone(),
            configuration,
            false,
            RendererOptions::default(),
        ));
        pollster::block_on(painter.set_window(ViewportId::ROOT, Some(window.clone())))
            .map_err(|error| format!("cannot draw the status window: {error}"))?;
        let state = egui_winit::State::new(
            context,
            ViewportId::ROOT,
            &window,
            Some(window.scale_factor() as f32),
            window.theme(),
            painter.max_texture_side(),
        );
        window.request_redraw();
        Ok(StatusWindow {
            window,
            state,
            painter,
            view,
            actions: Vec::new(),
        })
    }

    pub(crate) fn id(&self) -> WindowId {
        self.window.id()
    }

    /// Bring an already open window forward.
    pub(crate) fn raise(&self) {
        self.window.focus_window();
        self.window.request_redraw();
    }

    pub(crate) fn set_view(&mut self, view: View) {
        if view != self.view {
            self.view = view;
            self.window.request_redraw();
        }
    }

    pub(crate) fn take_actions(&mut self) -> Vec<Action> {
        std::mem::take(&mut self.actions)
    }

    /// Handle one window event; true when the person closed the window.
    pub(crate) fn on_event(&mut self, event: &WindowEvent) -> bool {
        match event {
            WindowEvent::CloseRequested => return true,
            WindowEvent::Resized(size) => {
                if let (Some(width), Some(height)) =
                    (NonZeroU32::new(size.width), NonZeroU32::new(size.height))
                {
                    self.painter
                        .on_window_resized(ViewportId::ROOT, width, height);
                }
            }
            WindowEvent::RedrawRequested => {
                self.paint();
                return false;
            }
            _ => {}
        }
        if self.state.on_window_event(&self.window, event).repaint {
            self.window.request_redraw();
        }
        false
    }

    fn paint(&mut self) {
        let input = self.state.take_egui_input(&self.window);
        let context = self.state.egui_ctx().clone();
        let (view, actions) = (&self.view, &mut self.actions);
        let mut pass = Vec::new();
        let output = context.run_ui(input, |ui| {
            // egui may run a discarded pass first; keep only the last one.
            pass.clear();
            draw(ui, view, &mut pass);
        });
        actions.append(&mut pass);
        self.state
            .handle_platform_output(&self.window, output.platform_output);
        let primitives = context.tessellate(output.shapes, output.pixels_per_point);
        let clear = egui::Rgba::from(context.global_style().visuals.panel_fill).to_array();
        self.painter.paint_and_update_textures(
            ViewportId::ROOT,
            output.pixels_per_point,
            clear,
            &primitives,
            &output.textures_delta,
            Vec::new(),
        );
        if output
            .viewport_output
            .get(&ViewportId::ROOT)
            .is_some_and(|viewport| viewport.repaint_delay.is_zero())
        {
            self.window.request_redraw();
        }
    }
}

fn control(ui: &mut egui::Ui, control: &Control, actions: &mut Vec<Action>) {
    let clicked = match control.checked {
        Some(checked) => {
            let mut shown = checked;
            ui.add_enabled(
                control.enabled,
                egui::Checkbox::new(&mut shown, control.label.as_str()),
            )
            .clicked()
        }
        None => ui
            .add_enabled(control.enabled, egui::Button::new(control.label.as_str()))
            .clicked(),
    };
    if clicked {
        actions.push(Action::Menu(control.id.clone()));
    }
}

fn draw(ui: &mut egui::Ui, view: &View, actions: &mut Vec<Action>) {
    egui::CentralPanel::default().show_inside(ui, |ui| {
        egui::ScrollArea::vertical().show(ui, |ui| {
            ui.heading("Erlkonig");
            ui.add_space(6.0);
            if let Some(prompt) = &view.prompt {
                ui.label(RichText::new(&prompt.title).strong());
                ui.add_space(4.0);
                ui.add(egui::Label::new(prompt.text.as_str()).selectable(true));
                ui.add_space(12.0);
                ui.horizontal_wrapped(|ui| {
                    for (index, button) in prompt.buttons.iter().enumerate() {
                        if ui.button(button.as_str()).clicked() {
                            actions.push(Action::Answer(index));
                        }
                    }
                });
                return;
            }
            if let Some(attention) = &view.attention {
                ui.colored_label(ui.visuals().warn_fg_color, attention.label.as_str());
            }
            let details = view.details.trim();
            if !details.is_empty() {
                ui.add(
                    egui::Label::new(RichText::new(details).monospace().small()).selectable(true),
                );
            }
            ui.separator();
            control(ui, &view.sync, actions);
            if let Some(retry) = &view.retry {
                control(ui, retry, actions);
            }
            ui.separator();
            ui.label(RichText::new("Integrations").strong());
            for row in &view.integrations {
                control(ui, row, actions);
            }
            ui.separator();
            control(ui, &view.storage, actions);
            control(ui, &view.login, actions);
            ui.separator();
            control(ui, &view.quit, actions);
        });
    });
}
