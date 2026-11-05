# GNOME support

## Current support

VOXL supports GNOME sessions on Xorg and Wayland. `--backend=auto` follows
`XDG_SESSION_TYPE`; `--backend=x11` and `--backend=wayland` select explicitly. Both
paths must run as the logged-in user in the graphical session.

The Linux build and library tests pass on Ubuntu 26.10. A live GNOME Wayland session
opened VOXL's RemoteDesktop screen-sharing consent dialog. Frame capture after consent
and input delivery have not yet been validated.

| Capability | GNOME on Xorg | GNOME on Wayland |
| --- | --- | --- |
| Screen capture | X11 MIT-SHM/GetImage and XDamage | XDG Desktop Portal ScreenCast + PipeWire; one selected monitor |
| Keyboard and pointer | XTEST injection | Portal RemoteDesktop, with user authorization |
| Clipboard | X11 CLIPBOARD selection and XFixes | Disabled; portal clipboard integration is not implemented |
| System audio | PulseAudio monitor source | PulseAudio-compatible monitor source, typically PipeWire's compatibility service |
| Dynamic resolution | RandR, when the X server supports it | Disabled; VOXL does not change the compositor's display mode |

The Xorg route can be launched from the graphical session with:

```sh
voxl --port=6901
```

If `DISPLAY` is not inherited by the process, set it to the active Xorg display and
preserve the session's X authorization environment. Do not copy authorization cookies
into a broadly readable environment or file.

## Wayland behavior and limitations

VOXL starts a combined RemoteDesktop/ScreenCast portal session, asks for one monitor,
opens the portal-provided PipeWire remote, and reads BGRx frames from GStreamer's
PipeWire source. The existing H.264 encoder and browser protocol are reused. The logged-in
user must approve the portal request. `--read-only` requests screen sharing without
keyboard or pointer permission.

The portal embeds the cursor in captured frames. Pointer coordinates are scaled from
PipeWire pixel dimensions to compositor logical dimensions, then clamped to the selected
monitor. Portal cancellation or a PipeWire capture failure stops the capture path with an
error. VOXL limits captured frames to 32 megapixels.

Clipboard synchronization and dynamic resolution are unavailable in the Wayland
backend. The server advertises the clipboard capability in its hello message so the
browser suppresses clipboard operations when disabled. GNOME users need
`xdg-desktop-portal-gnome`, a running PipeWire service, and the GStreamer PipeWire source
plugin (on Ubuntu, `gstreamer1.0-pipewire`). Linux builds need GStreamer 1.20 development
libraries and the plugin at runtime for Wayland capture.

## Reference interfaces

- [XDG Desktop Portal ScreenCast](https://flatpak.github.io/xdg-desktop-portal/docs/doc-org.freedesktop.portal.ScreenCast.html)
- [XDG Desktop Portal RemoteDesktop](https://flatpak.github.io/xdg-desktop-portal/docs/doc-org.freedesktop.portal.RemoteDesktop.html)
- [PipeWire Rust bindings](https://pipewire.pages.freedesktop.org/pipewire-rs/pipewire/)
