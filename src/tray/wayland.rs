//! Retain the app while eframe releases its Wayland toplevel and renderer.

use std::{
    collections::HashMap,
    fs,
    io::{self, Write as _},
    path::Path,
    sync::{Mutex, OnceLock},
    time::{Duration, Instant},
};

use eframe::App as _;
use raw_window_handle::{HasWindowHandle as _, RawWindowHandle};
use winit::{
    application::ApplicationHandler,
    event::WindowEvent,
    event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy},
    platform::run_on_demand::EventLoopExtRunOnDemand as _,
    window::WindowId,
};

use crate::ui::App;

static CONTEXT: Mutex<Option<egui::Context>> = Mutex::new(None);
static PROXY: OnceLock<EventLoopProxy<eframe::UserEvent>> = OnceLock::new();

pub(crate) fn wake() {
    let context = CONTEXT.lock().unwrap().clone();
    if let Some(context) = context {
        context.request_repaint_of(egui::ViewportId::ROOT);
    } else if let Some(proxy) = PROXY.get() {
        // The hidden handler uses this event only to wake the Wayland loop.
        let _ = proxy.send_event(eframe::UserEvent::RequestRepaint {
            viewport_id: egui::ViewportId::ROOT,
            when: Instant::now(),
            cumulative_pass_nr: 0,
        });
    }
}

fn retire_context() {
    let context = CONTEXT.lock().unwrap().take();
    if let Some(context) = context {
        // Dialogs and other in-flight work can still hold a context clone.
        // Forward their wakeups to the current window or the hidden loop.
        context.set_repaint_observer(|_| wake());
    }
}

pub(crate) fn run(
    first_options: eframe::NativeOptions,
    create: impl FnOnce(&eframe::CreationContext<'_>) -> anyhow::Result<App>,
) -> eframe::Result {
    // Match winit's backend selection, including the app's existing X11 override.
    let wayland = first_options.event_loop_builder.is_none()
        && ["WAYLAND_DISPLAY", "WAYLAND_SOCKET"]
            .into_iter()
            .any(|name| std::env::var(name).is_ok_and(|value| !value.is_empty()));
    if !wayland {
        let result = eframe::run_native(
            "BitChord",
            first_options,
            Box::new(|cc| {
                *CONTEXT.lock().unwrap() = Some(cc.egui_ctx.clone());
                let mut app = create(cc)?;
                if cc.window_handle().is_ok_and(|handle| {
                    matches!(
                        handle.as_raw(),
                        RawWindowHandle::Xlib(_) | RawWindowHandle::Xcb(_)
                    )
                }) {
                    app.start_tray(wake);
                }
                Ok(Box::new(app))
            }),
        );
        retire_context();
        return result;
    }

    let mut event_loop = EventLoop::with_user_event().build()?;
    let _ = PROXY.set(event_loop.create_proxy());
    let started = Instant::now();
    let options = first_options.clone();
    let mut geometry = None;
    let mut app = None;
    let mut creation_error = None;

    let mut result = run_window(
        &mut event_loop,
        first_options,
        Box::new({
            let (app, geometry, creation_error) = (&mut app, &mut geometry, &mut creation_error);
            move |cc| {
                *CONTEXT.lock().unwrap() = Some(cc.egui_ctx.clone());
                let mut created = create(cc).map_err(|err| {
                    let message = format!("{err:#}");
                    *creation_error = Some(err);
                    anyhow::anyhow!("{message}")
                })?;
                created.native_wayland = true;
                created.start_tray(wake);
                Ok(Box::new(NativeWindow {
                    app: app.insert(created),
                    geometry,
                    started,
                }))
            }
        }),
    );

    let Some(app) = app.as_mut() else {
        return result.and(Err(eframe::Error::AppCreation(
            creation_error
                .unwrap_or_else(|| anyhow::anyhow!("eframe failed to open the Wayland window"))
                .into(),
        )));
    };

    while result.is_ok() && app.wayland_detached() {
        result = event_loop
            .run_app_on_demand(&mut Hidden {
                app,
                context: egui::Context::default(),
                started,
            })
            .map_err(Into::into);
        if result.is_err() {
            break;
        }
        if app.wayland_exiting() {
            if let Some(path) = &options.persistence_path
                && let Err(err) = save_while_hidden(app, path)
            {
                log::warn!("Could not save playback session while hidden: {err:#}");
            }
            break;
        }

        let mut options = options.clone();
        let builder = app.wayland_viewport(options.viewport.clone());
        let previous = geometry;
        options.window_builder = Some(Box::new(move |restored| {
            let Some(previous) = previous else {
                return restored;
            };
            previous.restore(egui::ViewportBuilder {
                min_inner_size: builder.min_inner_size,
                max_inner_size: builder.max_inner_size,
                ..restored
            })
        }));
        result = run_window(
            &mut event_loop,
            options,
            Box::new({
                let (app, geometry) = (&mut *app, &mut geometry);
                move |cc| {
                    *CONTEXT.lock().unwrap() = Some(cc.egui_ctx.clone());
                    app.setup_window(cc);
                    cc.egui_ctx
                        .send_viewport_cmd(egui::ViewportCommand::RequestUserAttention(
                            egui::UserAttentionType::Informational,
                        ));
                    Ok(Box::new(NativeWindow {
                        app,
                        geometry,
                        started,
                    }))
                }
            }),
        );
    }

    if result.is_ok() && !app.wayland_exiting() {
        result = Err(eframe::Error::AppCreation(
            anyhow::anyhow!("eframe closed the Wayland window unexpectedly").into(),
        ));
    }
    app.on_exit(None);
    result
}

fn run_window(
    event_loop: &mut EventLoop<eframe::UserEvent>,
    options: eframe::NativeOptions,
    create: eframe::AppCreator<'_>,
) -> eframe::Result {
    let mut window = eframe::create_native("BitChord", options, create, event_loop);
    let result = event_loop.run_app_on_demand(&mut window);
    drop(window);
    retire_context();
    Ok(result?)
}

struct Hidden<'a> {
    app: &'a mut App,
    context: egui::Context,
    started: Instant,
}

impl ApplicationHandler<eframe::UserEvent> for Hidden<'_> {
    fn resumed(&mut self, _event_loop: &ActiveEventLoop) {}

    fn window_event(&mut self, _event_loop: &ActiveEventLoop, _id: WindowId, _event: WindowEvent) {}

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        // Keep dispatching Wayland requests, including compositor pings, with no
        // toplevel. The proxy wakes us for playback, tray events and launch IPC.
        let _ = self.context.run_logic(&egui::RawInput::default(), |ctx| {
            self.app
                .run_logic(ctx, self.started.elapsed().as_secs_f64(), false);
        });
        if self.app.wayland_detached() {
            event_loop.set_control_flow(ControlFlow::wait_duration(Duration::from_millis(100)));
        } else {
            event_loop.exit();
        }
    }
}

struct NativeWindow<'a> {
    app: &'a mut App,
    geometry: &'a mut Option<Geometry>,
    started: Instant,
}

impl eframe::App for NativeWindow<'_> {
    fn logic(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        // Wayland has no global window position. Capture the logical client size
        // directly because egui's inner rectangle also requires a position.
        if let Some(window) = frame.winit_window() {
            let pixels_per_point = (window.scale_factor() * f64::from(ctx.zoom_factor())).max(0.1);
            let size = window.inner_size().to_logical::<f32>(pixels_per_point);
            if size.width > 0.0 && size.height > 0.0 {
                let maximized = window.is_maximized();
                let fullscreen = window.fullscreen().is_some();
                let normal_size = if maximized || fullscreen {
                    self.geometry.and_then(|geometry| geometry.normal_size)
                } else {
                    Some(egui::vec2(size.width, size.height))
                };
                *self.geometry = Some(Geometry {
                    normal_size,
                    maximized,
                    fullscreen,
                });
            }
        }
        self.app
            .run_logic(ctx, self.started.elapsed().as_secs_f64(), true);
    }

    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        self.app.ui(ui, frame);
    }

    fn raw_input_hook(&mut self, ctx: &egui::Context, input: &mut egui::RawInput) {
        // eframe's clock starts over for each renderer. Keep one clock for the
        // queue, playback transitions and other timestamps held by the app.
        input.time = Some(self.started.elapsed().as_secs_f64());
        self.app.raw_input_hook(ctx, input);
    }

    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        self.app.save(storage);
    }

    fn on_exit(&mut self, gl: Option<&eframe::glow::Context>) {
        self.app.detach_wayland_window(gl);
    }

    fn clear_color(&self, visuals: &egui::Visuals) -> [f32; 4] {
        self.app.clear_color(visuals)
    }
}

#[derive(Clone, Copy)]
struct Geometry {
    normal_size: Option<egui::Vec2>,
    maximized: bool,
    fullscreen: bool,
}

impl Geometry {
    fn restore(self, mut builder: egui::ViewportBuilder) -> egui::ViewportBuilder {
        if let Some(size) = self.normal_size {
            builder = builder.with_inner_size(size);
        }
        builder
            .with_maximized(self.maximized)
            .with_fullscreen(self.fullscreen)
    }
}

// eframe has flushed and dropped its own storage before this runs. Read back
// its string map so a hidden Quit saves fresh app state and preserves egui keys.
fn save_while_hidden(app: &mut App, path: &Path) -> anyhow::Result<()> {
    let mut storage = Storage::read(path)?;
    app.save(&mut storage);
    storage.write(path)
}

#[derive(Default)]
struct Storage(HashMap<String, String>);

impl Storage {
    fn read(path: &Path) -> anyhow::Result<Self> {
        match fs::read(path) {
            Ok(content) => Ok(Self(ron::de::from_bytes(&content)?)),
            Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(Self::default()),
            Err(err) => Err(err.into()),
        }
    }

    fn write(&self, path: &Path) -> anyhow::Result<()> {
        let mut file =
            tempfile::NamedTempFile::new_in(path.parent().unwrap_or_else(|| Path::new(".")))?;
        ron::Options::default().to_io_writer_pretty(&mut file, &self.0, Default::default())?;
        file.flush()?;
        file.persist(path)?;
        Ok(())
    }
}

impl eframe::Storage for Storage {
    fn get_string(&self, key: &str) -> Option<String> {
        self.0.get(key).cloned()
    }

    fn set_string(&mut self, key: &str, value: String) {
        self.0.insert(key.to_owned(), value);
    }

    fn remove_string(&mut self, key: &str) {
        self.0.remove(key);
    }

    fn flush(&mut self) {}
}

#[cfg(test)]
mod tests {
    use super::*;
    use eframe::Storage as _;

    #[test]
    fn restored_window_keeps_last_normal_size_and_maximized_state() {
        let geometry = Geometry {
            normal_size: Some(egui::vec2(960.0, 640.0)),
            maximized: true,
            fullscreen: false,
        };
        let builder = geometry.restore(egui::ViewportBuilder::default());
        assert_eq!(builder.inner_size, geometry.normal_size);
        assert_eq!(builder.maximized, Some(true));
        assert_eq!(builder.fullscreen, Some(false));
    }

    #[test]
    fn hidden_save_updates_playback_and_preserves_window_and_egui_keys() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("app.ron");
        let mut storage = Storage::read(&path).unwrap();
        storage.set_string("session", "old position".into());
        storage.set_string("window", "normal size".into());
        storage.set_string("egui", "memory".into());
        storage.write(&path).unwrap();

        let mut storage = Storage::read(&path).unwrap();
        storage.set_string("session", "new position".into());
        storage.write(&path).unwrap();

        let storage = Storage::read(&path).unwrap();
        assert_eq!(
            storage.get_string("session").as_deref(),
            Some("new position")
        );
        assert_eq!(storage.get_string("window").as_deref(), Some("normal size"));
        assert_eq!(storage.get_string("egui").as_deref(), Some("memory"));
    }
}
