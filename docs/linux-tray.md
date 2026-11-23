# Linux system tray

BitChord uses tray-icon 0.26.1's StatusNotifierItem (KSNI) backend on the session
D-Bus. The desktop needs a tray host that implements StatusNotifierItem and
DBusMenu. The app checks the watcher's host property before it allows the window
to hide. If the watcher or host goes away, BitChord restores a window it had hidden.

| Desktop session | Requirements |
| --- | --- |
| KDE Plasma on X11 or Wayland | The Plasma System Tray widget. |
| GNOME on X11 or Wayland | An enabled AppIndicator/KStatusNotifierItem extension. |
| niri and wlroots compositors | An SNI tray host, such as a configured Waybar tray module. |
| X11 panel with only XEmbed | An SNI-capable panel or bridge is required. |

BitChord selects X11 when `DISPLAY` is available, including inside a Wayland
session. Launch with `BITCHORD_NATIVE_WAYLAND=1` to use native Wayland in a
Wayland session with XWayland. A Wayland-only session uses Wayland automatically.
GTK3 and WebKitGTK remain dependencies for the webview helpers. No
libappindicator shared library is required for the Linux tray.

The menu has one window action (**Hide** when visible, **Show** when hidden or
minimized) and **Quit**. Left-click restores or focuses BitChord when the panel
sends SNI Activate. Some panels open the menu themselves. The panel controls
which click opens it and the window manager can reject focus requests. The
Linux **Close to tray** setting was removed because closing uses the tray
automatically whenever one is ready.

The desktop launcher declares the `bitchord:` URL scheme. A plain launch and a
party link are handed to the existing window for the same data profile.

## Native Wayland window lifecycle

In winit 0.30.13, Wayland ignores `set_visible` and the request to unminimize.
That also limits egui's `ViewportCommand::Visible`, including a hidden root
viewport. Native Wayland Hide closes the toplevel and releases its renderer.
BitChord keeps the same app, audio, tray, and profile listener alive, and the
Wayland event loop keeps dispatching while the window is absent. Show,
left activation, a second launch, a party link, or loss of the tray host
opens a new toplevel with the same app state. Quit saves the current playback
session whether a window exists or not.

Show requests attention through winit's xdg-activation support when available.
The compositor decides placement, sizing, and focus. SNI Activate and menu
events carry no Wayland input serial or activation token. Wayland also exposes
no portable minimized-state query. Use Hide to put BitChord in the tray.

## Audit

- **P1, window lifecycle.** Tray ownership follows the process. Visible,
  minimized and app-hidden states follow the window state machine. On Wayland
  the app-hidden state spans the gap between toplevels. Each event batch is
  consumed and Quit wins over pending Show, Hide or external activation.
- **P1, native event loops.** The previous AppIndicator path could schedule GTK
  work on the wrong thread and wait for it on the egui thread. KSNI exports the
  icon and menu over D-Bus and delivers Activate events. The worker owns setup
  and teardown; the process waits for teardown with a deadline after eframe
  returns. Sign-in and lyrics verification still initialize GTK in their own
  child processes.
- **P1, exit and activation.** The Linux profile lock prevents concurrent launches
  from creating duplicate windows. On Quit BitChord saves state before it
  stops playback. Linux sign-in, verification and yt-dlp helpers run in process
  groups that BitChord signals and reaps during shutdown.
- **P2, tray presence.** The SNI watcher advertises a host. Its property cannot
  tell whether the panel has individually suppressed BitChord in its settings.
  A second launch can still restore the existing window.
- **P2, window geometry and focus.** X11 restoration uses a delayed focus retry
  so the window manager has time to map the window. Native Wayland applies
  the previous client size and maximize or fullscreen state to the new toplevel.
  Test restore with maximized windows, changed monitor layouts and the mini
  player on the target window manager.
- **P2, repository CI.** Repository-wide formatting drift and strict Clippy
  diagnostics are present on the base commit.
- **P3, external apps.** Programs that BitChord asks the desktop to open, such
  as a browser or file manager, have their own lifetimes. Quit only manages
  BitChord's playback and app-owned helpers.

## Verification on a desktop

In an X11 or XWayland session with an SNI tray, start playback. Check Hide, Show,
the window close button, minimization, and left activation. Confirm the icon
persists and the menu changes without an extra window. Hide and launch BitChord
again with the same profile, then do it again with a `bitchord://party/ABC234`
argument. Test Quit while hidden and while a helper is open. Confirm playback
stops and reopening retains the session. Disable the tray host while hidden
and confirm the window returns. With the host disabled at startup, confirm the
close button exits.

On niri, repeat these checks with `BITCHORD_NATIVE_WAYLAND=1` and an SNI host.
Use tray Hide in place of the platform minimize test. Check that playback
advances to the next track while no BitChord window is present. Repeat Hide
and Show several times, checking the compositor's window list for zero
BitChord windows while hidden and one after Show. Repeat with both OpenGL and
wgpu, and verify that Quit while hidden saves the latest playback position.

The separate IPC integration test needs permission to create UNIX-domain sockets
and can run without a desktop.

```sh
cargo test --locked --offline launch_requests_reach_the_same_locked_profile -- --ignored
```
