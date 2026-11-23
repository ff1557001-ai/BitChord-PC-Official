//! Window state is independent of the tray and outlives any menu event batch.

use super::Cmd;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Visibility {
    #[default]
    Visible,
    Minimized,
    Hidden,
}

#[derive(Debug, Default)]
pub struct Window {
    visibility: Visibility,
    exiting: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    None,
    Hide,
    Show,
    Quit,
}

impl Window {
    pub fn hidden(&self) -> bool {
        self.visibility != Visibility::Visible
    }

    #[cfg(target_os = "linux")]
    pub fn menu_action(&self) -> Cmd {
        if self.hidden() { Cmd::Show } else { Cmd::Hide }
    }

    #[cfg(target_os = "linux")]
    pub fn detached(&self) -> bool {
        self.visibility == Visibility::Hidden && !self.exiting
    }

    pub fn exiting(&self) -> bool {
        self.exiting
    }

    pub fn quit(&mut self) {
        self.exiting = true;
    }

    /// A Wayland hide closes the toplevel. A pending Show or a lost host
    /// can still cancel the close before eframe has destroyed the window.
    #[cfg(target_os = "linux")]
    pub fn update_wayland(
        &mut self,
        close: bool,
        can_hide: bool,
        reveal: bool,
        commands: impl IntoIterator<Item = Cmd>,
    ) -> (Action, bool) {
        let detaching = self.detached();
        let action = self.update(None, close && !detaching, can_hide, reveal, commands);
        let cancel_close = close && action == Action::Show;
        (action, cancel_close)
    }

    pub fn update(
        &mut self,
        minimized: Option<bool>,
        close: bool,
        can_hide: bool,
        reveal: bool,
        commands: impl IntoIterator<Item = Cmd>,
    ) -> Action {
        if self.exiting {
            return Action::None;
        }
        if self.visibility != Visibility::Hidden
            && let Some(minimized) = minimized
        {
            self.visibility = if minimized {
                Visibility::Minimized
            } else {
                Visibility::Visible
            };
        }

        // Explicit menu intents are idempotent. The last one wins unless any
        // command requests Quit. A link or second launch beats Hide.
        let mut action = Action::None;
        for cmd in commands {
            action = match (action, cmd) {
                (Action::Quit, _) | (_, Cmd::Quit) => Action::Quit,
                (_, Cmd::Show) => Action::Show,
                #[cfg(target_os = "linux")]
                (_, Cmd::Hide) => Action::Hide,
            };
        }
        if action == Action::Quit || (close && !can_hide) {
            self.quit();
            return Action::Quit;
        }
        if reveal || action == Action::Show || (!can_hide && self.visibility == Visibility::Hidden)
        {
            self.visibility = Visibility::Visible;
            return Action::Show;
        }
        if can_hide && (close || action == Action::Hide) {
            self.visibility = Visibility::Hidden;
            return Action::Hide;
        }
        Action::None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn close_hides_and_show_restores_the_same_window() {
        let mut window = Window::default();
        for _ in 0..3 {
            assert_eq!(
                window.update(Some(false), true, true, false, []),
                Action::Hide
            );
            assert!(window.hidden());
            assert!(!window.exiting());
            assert_eq!(
                window.update(Some(false), false, true, false, []),
                Action::None
            );
            assert!(
                window.hidden(),
                "a native minimize report cannot unhide the window"
            );
            assert_eq!(
                window.update(None, false, true, false, [Cmd::Show]),
                Action::Show
            );
            assert!(!window.hidden());
        }
    }

    #[test]
    fn failed_or_pending_tray_leaves_close_enabled() {
        let mut window = Window::default();
        assert_eq!(window.update(None, false, false, false, []), Action::None);
        assert_eq!(window.update(None, true, false, false, []), Action::Quit);
        assert!(!window.hidden());
        assert!(window.exiting());
    }

    #[test]
    fn a_lost_host_recovers_a_hidden_window() {
        let mut window = Window::default();
        window.update(None, true, true, false, []);
        assert_eq!(window.update(None, false, false, false, []), Action::Show);
        assert!(!window.hidden());
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn menu_action_tracks_native_minimize_and_restore() {
        let mut window = Window::default();
        assert_eq!(window.menu_action(), Cmd::Hide);
        window.update(Some(true), false, true, false, []);
        assert_eq!(window.menu_action(), Cmd::Show);
        window.update(Some(false), false, true, false, []);
        assert_eq!(window.menu_action(), Cmd::Hide);
        window.update(Some(true), false, true, false, []);
        assert_eq!(
            window.update(Some(true), false, true, false, [Cmd::Show]),
            Action::Show
        );
        assert_eq!(window.menu_action(), Cmd::Hide);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn last_visibility_command_wins_without_quit() {
        let mut window = Window::default();
        assert_eq!(
            window.update(None, false, true, false, [Cmd::Show, Cmd::Hide, Cmd::Hide]),
            Action::Hide
        );
        assert!(window.hidden());
        assert_eq!(
            window.update(None, false, true, false, [Cmd::Hide, Cmd::Show]),
            Action::Show
        );
        assert!(!window.hidden());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn deep_link_or_later_launch_restores_even_with_pending_hide() {
        let mut window = Window::default();
        window.update(None, true, true, false, []);
        assert_eq!(
            window.update(None, false, true, true, [Cmd::Hide]),
            Action::Show
        );
        assert!(!window.hidden());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn wayland_close_and_hide_detach_until_show() {
        let mut window = Window::default();
        assert_eq!(
            window.update_wayland(true, true, false, []),
            (Action::Hide, false)
        );
        assert!(window.detached());
        assert_eq!(window.menu_action(), Cmd::Show);
        assert_eq!(
            window.update_wayland(false, true, false, [Cmd::Show]),
            (Action::Show, false)
        );
        assert!(!window.detached());
        assert_eq!(window.menu_action(), Cmd::Hide);
        assert_eq!(
            window.update_wayland(false, true, false, [Cmd::Hide]),
            (Action::Hide, false)
        );
        assert_eq!(
            window.update_wayland(true, true, false, []),
            (Action::None, false)
        );
        assert!(window.detached());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn wayland_reveal_or_host_loss_can_cancel_a_pending_detach() {
        for (host, reveal) in [(true, true), (false, false)] {
            let mut window = Window::default();
            window.update_wayland(false, true, false, [Cmd::Hide]);
            assert_eq!(
                window.update_wayland(true, host, reveal, []),
                (Action::Show, true)
            );
            assert!(!window.detached());
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn wayland_quit_wins_while_detached_and_close_exits_without_a_host() {
        let mut window = Window::default();
        window.update_wayland(true, true, false, []);
        assert_eq!(
            window.update_wayland(false, true, true, [Cmd::Show, Cmd::Quit]),
            (Action::Quit, false)
        );
        assert!(window.exiting());
        assert!(!window.detached());

        let mut window = Window::default();
        assert_eq!(
            window.update_wayland(true, false, false, []),
            (Action::Quit, false)
        );
    }

    #[test]
    fn quit_consumes_the_batch_and_is_terminal() {
        let mut window = Window::default();
        window.update(None, true, true, false, []);
        let mut drained = 0;
        let commands = [Cmd::Show, Cmd::Quit, Cmd::Show, Cmd::Quit]
            .into_iter()
            .inspect(|_| drained += 1);
        assert_eq!(
            window.update(None, true, true, true, commands),
            Action::Quit
        );
        assert_eq!(drained, 4);
        assert!(
            window.hidden(),
            "shutdown retains the geometry-saving state"
        );
        assert!(window.exiting());
        assert_eq!(
            window.update(Some(false), false, true, true, [Cmd::Show]),
            Action::None
        );
    }
}
