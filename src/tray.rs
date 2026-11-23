//! The Linux tray lives on its own worker thread until app shutdown. Windows and
//! macOS create it on the UI thread only when closing to the tray.

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
pub(crate) mod wayland;
mod window;

use std::sync::{
    Arc, Mutex, Once,
    atomic::{AtomicU64, Ordering},
};

use crossbeam_channel::{Receiver, Sender};
use tray_icon::menu::{Menu, MenuEvent, MenuItem};
use tray_icon::{
    MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent, TrayIconId,
};

#[cfg(target_os = "linux")]
pub use linux::finish;
pub use window::{Action, Window};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cmd {
    #[cfg(target_os = "linux")]
    Hide,
    Show,
    Quit,
}

struct Events {
    id: TrayIconId,
    toggle_id: String,
    quit_id: String,
    toggle: Cmd,
    tx: Sender<Cmd>,
    wake: Arc<dyn Fn() + Send + Sync>,
}

static EVENTS: Mutex<Option<Events>> = Mutex::new(None);

pub struct Tray {
    id: TrayIconId,
    rx: Receiver<Cmd>,
    #[cfg(not(target_os = "linux"))]
    _icon: TrayIcon,
    #[cfg(target_os = "linux")]
    worker: linux::Worker,
}

impl Tray {
    pub fn new(
        icon: &'static crate::appicon::Variant,
        wake: impl Fn() + Send + Sync + Clone + 'static,
    ) -> anyhow::Result<Self> {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        static HANDLERS: Once = Once::new();
        HANDLERS.call_once(|| {
            TrayIconEvent::set_event_handler(Some(|event| {
                if let TrayIconEvent::Click {
                    id,
                    button: MouseButton::Left,
                    button_state: MouseButtonState::Up,
                    ..
                } = event
                {
                    dispatch(|e| (e.id == id).then_some(Cmd::Show));
                }
            }));
            MenuEvent::set_event_handler(Some(|event: MenuEvent| {
                dispatch(|e| {
                    if event.id.as_ref() == e.toggle_id {
                        Some(e.toggle)
                    } else if event.id.as_ref() == e.quit_id {
                        Some(Cmd::Quit)
                    } else {
                        None
                    }
                });
            }));
        });

        let serial = NEXT.fetch_add(1, Ordering::Relaxed);
        let id = TrayIconId::new(format!("bitchord-{serial}"));
        let toggle_id = format!("bitchord-window-{serial}");
        let quit_id = format!("bitchord-quit-{serial}");
        #[cfg(target_os = "linux")]
        let toggle = Cmd::Hide;
        #[cfg(not(target_os = "linux"))]
        let toggle = Cmd::Show;
        let (tx, rx) = crossbeam_channel::unbounded();
        #[cfg(target_os = "linux")]
        let worker = linux::Worker::start(
            icon,
            id.clone(),
            toggle_id.clone(),
            quit_id.clone(),
            wake.clone(),
        )?;
        #[cfg(not(target_os = "linux"))]
        let (_icon, _) = build(icon, id.clone(), &toggle_id, &quit_id, toggle)?;

        *EVENTS.lock().unwrap() = Some(Events {
            id: id.clone(),
            toggle_id,
            quit_id,
            toggle,
            tx,
            wake: Arc::new(wake),
        });
        Ok(Self {
            id,
            rx,
            #[cfg(not(target_os = "linux"))]
            _icon,
            #[cfg(target_os = "linux")]
            worker,
        })
    }

    pub fn take(&self) -> impl Iterator<Item = Cmd> + '_ {
        self.rx.try_iter().take(self.rx.len())
    }

    #[cfg(target_os = "linux")]
    pub fn available(&self) -> bool {
        self.worker.available()
    }

    #[cfg(target_os = "linux")]
    pub fn set_menu_action(&self, cmd: Cmd) {
        self.worker.set_toggle(cmd);
    }
}

impl Drop for Tray {
    fn drop(&mut self) {
        let mut events = EVENTS.lock().unwrap();
        if events.as_ref().is_some_and(|e| e.id == self.id) {
            events.take();
        }
    }
}

impl Cmd {
    fn label(self) -> &'static str {
        match self {
            #[cfg(target_os = "linux")]
            Self::Hide => "Hide",
            #[cfg(target_os = "linux")]
            Self::Show => "Show",
            #[cfg(not(target_os = "linux"))]
            Self::Show => "Show YT Music",
            Self::Quit => "Quit",
        }
    }
}

fn build(
    icon: &crate::appicon::Variant,
    id: TrayIconId,
    toggle_id: &str,
    quit_id: &str,
    toggle: Cmd,
) -> anyhow::Result<(TrayIcon, MenuItem)> {
    let png = icon
        .rgba(32)
        .ok_or_else(|| anyhow::anyhow!("icon doesn't decode"))?;
    let (w, h) = png.dimensions();
    let toggle = MenuItem::with_id(toggle_id, toggle.label(), true, None);
    let quit = MenuItem::with_id(quit_id, Cmd::Quit.label(), true, None);
    let menu = Menu::with_items(&[&toggle, &quit])?;
    let builder = TrayIconBuilder::new()
        .with_id(id)
        .with_icon(tray_icon::Icon::from_rgba(png.into_raw(), w, h)?)
        .with_menu(Box::new(menu))
        .with_menu_on_left_click(false);
    #[cfg(target_os = "linux")]
    let builder = builder.with_title("BitChord");
    #[cfg(not(target_os = "linux"))]
    let builder = builder.with_tooltip("YT Music");
    Ok((builder.build()?, toggle))
}

fn dispatch(command: impl FnOnce(&Events) -> Option<Cmd>) {
    let wake = EVENTS.lock().unwrap().as_ref().and_then(|events| {
        let cmd = command(events)?;
        events.tx.send(cmd).ok()?;
        Some(events.wake.clone())
    });
    if let Some(wake) = wake {
        wake();
    }
}
