use std::sync::{
    Arc, OnceLock,
    atomic::{AtomicBool, Ordering},
};
use std::time::Duration;

use gio::glib;
use glib::variant::ToVariant;
use tray_icon::TrayIconId;

use super::Cmd;

enum Message {
    SetToggle(Cmd),
    Stop,
}

static DONE: OnceLock<crossbeam_channel::Receiver<()>> = OnceLock::new();

pub(super) struct Worker {
    send: glib::Sender<Message>,
    available: Arc<AtomicBool>,
}

impl Worker {
    pub(super) fn start(
        icon: &'static crate::appicon::Variant,
        id: TrayIconId,
        toggle_id: String,
        quit_id: String,
        wake: impl Fn() + Send + 'static,
    ) -> anyhow::Result<Self> {
        // GLib 0.18's channel wakes the worker loop without polling the UI.
        #[allow(deprecated)]
        let (send, recv) = glib::MainContext::channel(glib::Priority::DEFAULT);
        let (done, finished) = crossbeam_channel::bounded(0);
        anyhow::ensure!(DONE.set(finished).is_ok(), "Linux tray already started");
        let available = Arc::new(AtomicBool::new(false));
        let ready = available.clone();
        std::thread::Builder::new()
            .name("linux-tray".into())
            .spawn(move || {
                // KSNI's blocking setup and teardown stay off the egui thread.
                match super::build(icon, id, &toggle_id, &quit_id, Cmd::Hide) {
                    Ok((tray, toggle)) => {
                        let context = glib::MainContext::new();
                        let main_loop = glib::MainLoop::new(Some(&context), false);
                        let stop = main_loop.clone();
                        let source = recv.attach(Some(&context), move |message| {
                            match message {
                                Message::SetToggle(cmd) => {
                                    if let Some(events) = super::EVENTS.lock().unwrap().as_mut() {
                                        events.toggle = cmd;
                                    }
                                    toggle.set_text(cmd.label());
                                }
                                Message::Stop => stop.quit(),
                            }
                            glib::ControlFlow::Continue
                        });
                        let watch = context.spawn_local(watch_host(ready.clone(), wake));
                        if let Err(err) = context.with_thread_default(|| main_loop.run()) {
                            log::warn!("tray event loop unavailable: {err}");
                        }
                        watch.abort();
                        if let Some(source) = context.find_source_by_id(&source) {
                            source.destroy();
                        }
                        drop(tray);
                    }
                    Err(err) => log::warn!("tray icon unavailable: {err:#}"),
                }
                ready.store(false, Ordering::Release);
                drop(done);
            })?;
        Ok(Self { send, available })
    }

    pub(super) fn available(&self) -> bool {
        self.available.load(Ordering::Acquire)
    }

    pub(super) fn set_toggle(&self, cmd: Cmd) {
        let _ = self.send.send(Message::SetToggle(cmd));
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        let _ = self.send.send(Message::Stop);
    }
}

/// Wait outside the egui event loop, including when initialization failed or the
/// eframe window closed normally. A stalled tray startup cannot hold up exit.
pub fn finish() {
    if let Some(done) = DONE.get()
        && done.recv_timeout(Duration::from_secs(2))
            == Err(crossbeam_channel::RecvTimeoutError::Timeout)
    {
        log::warn!("Linux tray shutdown timed out");
    }
}

async fn watch_host(ready: Arc<AtomicBool>, wake: impl Fn()) {
    const WATCHER: &str = "org.kde.StatusNotifierWatcher";
    // A successful SNI registration alone says nothing about a panel.
    let Ok(bus) = gio::bus_get_future(gio::BusType::Session).await else {
        return;
    };
    // Losing the session bus should expose the window, then allow normal close.
    bus.set_exit_on_close(false);
    loop {
        let available = bus
            .call_future(
                Some(WATCHER),
                "/StatusNotifierWatcher",
                "org.freedesktop.DBus.Properties",
                "Get",
                Some(&(WATCHER, "IsStatusNotifierHostRegistered").to_variant()),
                None,
                gio::DBusCallFlags::NO_AUTO_START,
                1_000,
            )
            .await
            .ok()
            .and_then(|reply| reply.get::<(glib::Variant,)>())
            .and_then(|(value,)| value.get::<bool>())
            .unwrap_or(false);
        if ready.swap(available, Ordering::AcqRel) != available {
            wake();
        }
        if bus.is_closed() {
            return;
        }
        glib::timeout_future_seconds(2).await;
    }
}
