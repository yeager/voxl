//! Portal session for a local Wayland desktop.
//!
//! The portal is the compositor-authorized boundary for screen sharing and remote
//! input. This module deliberately does not use Xwayland for desktop-wide capture.

use ashpd::desktop::remote_desktop::{
    DeviceType, KeyState, NotifyKeyboardKeysymOptions, NotifyPointerAxisOptions,
    NotifyPointerButtonOptions, NotifyPointerMotionAbsoluteOptions, RemoteDesktop,
    SelectDevicesOptions,
};
use ashpd::desktop::screencast::{
    CursorMode, OpenPipeWireRemoteOptions, Screencast, SelectSourcesOptions, SourceType,
};
use ashpd::desktop::Session;
use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app as gst_app;
use gstreamer_video as gst_video;
use std::collections::HashSet;
use std::os::fd::{AsRawFd, OwnedFd};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc;
use std::sync::Arc;
use std::thread;
use std::time::Duration;

pub struct SessionHandle {
    remote: RemoteDesktop,
    session: Session<RemoteDesktop>,
    _pipewire_remote: OwnedFd,
    pub node_id: u32,
    pub logical_x: i32,
    pub logical_y: i32,
    pub logical_width: u32,
    pub logical_height: u32,
    capture_width: AtomicU32,
    capture_height: AtomicU32,
}

pub struct Frame {
    pub data: Vec<u8>,
    pub width: u32,
    pub height: u32,
    pub stride: usize,
}

/// GStreamer consumer for the PipeWire remote granted by the portal.
pub struct Capture {
    pipeline: gst::Pipeline,
    sink: gst_app::AppSink,
}

impl Capture {
    pub fn new(session: &SessionHandle) -> Result<Self, String> {
        gst::init().map_err(|e| format!("GStreamer initialization failed: {e}"))?;

        let source = gst::ElementFactory::make("pipewiresrc")
            .property("fd", session._pipewire_remote.as_raw_fd())
            .property("path", session.node_id.to_string())
            .property("do-timestamp", true)
            .build()
            .map_err(|e| {
                format!("could not create pipewiresrc (install the GStreamer PipeWire plugin): {e}")
            })?;
        let caps = gst::Caps::builder("video/x-raw")
            .field("format", "BGRx")
            .build();
        let filter = gst::ElementFactory::make("capsfilter")
            .property("caps", &caps)
            .build()
            .map_err(|e| format!("could not create GStreamer capsfilter: {e}"))?;
        let sink = gst_app::AppSink::builder()
            .max_buffers(1)
            .drop(true)
            .sync(false)
            .build();
        let pipeline = gst::Pipeline::new();
        pipeline
            .add(&source)
            .and_then(|_| pipeline.add(&filter))
            .and_then(|_| pipeline.add(&sink))
            .map_err(|e| format!("could not assemble PipeWire capture pipeline: {e}"))?;
        source
            .link(&filter)
            .and_then(|_| filter.link(&sink))
            .map_err(|e| format!("could not link PipeWire capture pipeline: {e}"))?;
        pipeline
            .set_state(gst::State::Playing)
            .map_err(|e| format!("could not start PipeWire capture pipeline: {e:?}"))?;
        Ok(Self { pipeline, sink })
    }

    /// Return the next available frame, with a bounded wait. The PipeWire plugin
    /// drops stale buffers so a slow encoder does not build an unbounded queue.
    pub fn pull(&self, timeout: Duration) -> Result<Option<Frame>, String> {
        let timeout =
            gst::ClockTime::from_mseconds(timeout.as_millis().min(u64::MAX as u128) as u64);
        let Some(sample) = self.sink.try_pull_sample(timeout) else {
            return Ok(None);
        };
        let caps = sample
            .caps()
            .ok_or("PipeWire frame has no negotiated caps")?;
        let info = gst_video::VideoInfo::from_caps(caps)
            .map_err(|e| format!("invalid PipeWire video format: {e}"))?;
        let buffer = sample
            .buffer()
            .ok_or("PipeWire sample has no frame buffer")?;
        let map = buffer
            .map_readable()
            .map_err(|e| format!("could not map PipeWire frame: {e}"))?;
        let width = info.width();
        let height = info.height();
        let stride = info.stride()[0] as usize;
        let required = stride
            .checked_mul(height as usize)
            .ok_or("PipeWire frame dimensions overflow")?;
        let offset = info.offset()[0] as usize;
        let end = offset
            .checked_add(required)
            .ok_or("PipeWire frame offset overflow")?;
        if width == 0
            || height == 0
            || (width as u64) * (height as u64) > 33_554_432
            || map.as_slice().len() < end
        {
            return Err("PipeWire returned a truncated or empty frame".into());
        }
        Ok(Some(Frame {
            data: map.as_slice()[offset..end].to_vec(),
            width,
            height,
            stride,
        }))
    }
}

impl Drop for Capture {
    fn drop(&mut self) {
        let _ = self.pipeline.set_state(gst::State::Null);
    }
}

impl SessionHandle {
    /// Request a single monitor stream and keyboard/pointer access. The portal's
    /// consent dialog is expected and must be completed by the logged-in user.
    pub async fn start(read_only: bool) -> Result<Arc<Self>, String> {
        let remote = RemoteDesktop::new()
            .await
            .map_err(|e| format!("RemoteDesktop portal unavailable: {e}"))?;
        let screencast = Screencast::new()
            .await
            .map_err(|e| format!("ScreenCast portal unavailable: {e}"))?;
        let session = remote
            .create_session(Default::default())
            .await
            .map_err(|e| format!("could not create portal session: {e}"))?;
        remote
            .select_devices(
                &session,
                SelectDevicesOptions::default().set_devices(if read_only {
                    DeviceType::default()
                } else {
                    DeviceType::KEYBOARD | DeviceType::POINTER
                }),
            )
            .await
            .map_err(|e| format!("could not request keyboard and pointer access: {e}"))?;
        screencast
            .select_sources(
                &session,
                SelectSourcesOptions::default()
                    .set_cursor_mode(CursorMode::EMBEDDED)
                    .set_sources(SourceType::MONITOR)
                    .set_multiple(false),
            )
            .await
            .map_err(|e| format!("could not select a monitor for sharing: {e}"))?;

        let selected = remote
            .start(&session, None, Default::default())
            .await
            .map_err(|e| format!("portal session start failed: {e}"))?
            .response()
            .map_err(|e| format!("screen/input permission was denied or cancelled: {e}"))?;
        let stream = selected
            .streams()
            .first()
            .ok_or("portal granted no monitor stream")?;
        let (width, height) = stream
            .size()
            .ok_or("portal did not report stream dimensions")?;
        if width <= 0 || height <= 0 || (width as u64) * (height as u64) > 33_554_432 {
            return Err(format!(
                "portal stream size {width}x{height} is invalid or exceeds the 32-megapixel limit"
            ));
        }
        let (logical_x, logical_y) = stream.position().unwrap_or((0, 0));
        let pipewire_remote = screencast
            .open_pipe_wire_remote(&session, OpenPipeWireRemoteOptions::default())
            .await
            .map_err(|e| format!("could not open the portal PipeWire remote: {e}"))?;

        log::info!(
            "Wayland portal authorized monitor {} at {}x{}",
            stream.pipe_wire_node_id(),
            width,
            height
        );
        Ok(Arc::new(Self {
            remote,
            session,
            _pipewire_remote: pipewire_remote,
            node_id: stream.pipe_wire_node_id(),
            logical_x,
            logical_y,
            logical_width: width as u32,
            logical_height: height as u32,
            capture_width: AtomicU32::new(width as u32),
            capture_height: AtomicU32::new(height as u32),
        }))
    }

    pub async fn pointer(&self, x: i32, y: i32) -> Result<(), String> {
        let frame_width = self.capture_width.load(Ordering::Relaxed).max(1);
        let frame_height = self.capture_height.load(Ordering::Relaxed).max(1);
        let (x, y) = map_pointer(
            x,
            y,
            frame_width,
            frame_height,
            self.logical_width,
            self.logical_height,
        );
        self.remote
            .notify_pointer_motion_absolute(
                &self.session,
                self.node_id,
                f64::from(self.logical_x) + f64::from(x),
                f64::from(self.logical_y) + f64::from(y),
                NotifyPointerMotionAbsoluteOptions::default(),
            )
            .await
            .map_err(|e| format!("portal pointer motion failed: {e}"))
    }

    pub fn set_capture_dimensions(&self, width: u32, height: u32) {
        self.capture_width.store(width.max(1), Ordering::Relaxed);
        self.capture_height.store(height.max(1), Ordering::Relaxed);
    }

    pub async fn button(&self, button: u8, down: bool) -> Result<(), String> {
        let button = linux_button_code(button)
            .ok_or_else(|| format!("unsupported remote pointer button {button}"))?;
        self.remote
            .notify_pointer_button(
                &self.session,
                button,
                if down {
                    KeyState::Pressed
                } else {
                    KeyState::Released
                },
                NotifyPointerButtonOptions::default(),
            )
            .await
            .map_err(|e| format!("portal pointer button failed: {e}"))
    }

    pub async fn scroll(&self, dx: f64, dy: f64) -> Result<(), String> {
        self.remote
            .notify_pointer_axis(&self.session, dx, dy, NotifyPointerAxisOptions::default())
            .await
            .map_err(|e| format!("portal scroll failed: {e}"))
    }

    pub async fn key(&self, keysym: u32, down: bool) -> Result<(), String> {
        let keysym = i32::try_from(keysym).map_err(|_| "keysym is outside the portal range")?;
        self.remote
            .notify_keyboard_keysym(
                &self.session,
                keysym,
                if down {
                    KeyState::Pressed
                } else {
                    KeyState::Released
                },
                NotifyKeyboardKeysymOptions::default(),
            )
            .await
            .map_err(|e| format!("portal keyboard event failed: {e}"))
    }
}

fn clamp_pointer(x: i32, y: i32, width: u32, height: u32) -> (i32, i32) {
    (
        x.clamp(0, width.saturating_sub(1).min(i32::MAX as u32) as i32),
        y.clamp(0, height.saturating_sub(1).min(i32::MAX as u32) as i32),
    )
}

fn map_pointer(
    x: i32,
    y: i32,
    frame_width: u32,
    frame_height: u32,
    logical_width: u32,
    logical_height: u32,
) -> (i32, i32) {
    let (x, y) = clamp_pointer(x, y, frame_width, frame_height);
    let x = (u64::from(x as u32) * u64::from(logical_width) / u64::from(frame_width.max(1))) as i32;
    let y =
        (u64::from(y as u32) * u64::from(logical_height) / u64::from(frame_height.max(1))) as i32;
    let (x, y) = clamp_pointer(x, y, logical_width, logical_height);
    (x, y)
}

/// The web client uses X11 button numbers. The portal wants Linux evdev button codes.
fn linux_button_code(button: u8) -> Option<i32> {
    match button {
        1 => Some(0x110), // BTN_LEFT
        2 => Some(0x112), // BTN_MIDDLE
        3 => Some(0x111), // BTN_RIGHT
        8 => Some(0x116), // BTN_SIDE
        9 => Some(0x115), // BTN_EXTRA
        _ => None,
    }
}

pub fn spawn_input(
    session: Arc<SessionHandle>,
    px_per_notch: f64,
) -> mpsc::Sender<crate::input::InputEvent> {
    let (tx, rx) = mpsc::channel();
    thread::Builder::new()
        .name("wayland-input".into())
        .spawn(move || {
            let runtime = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(runtime) => runtime,
                Err(e) => {
                    log::error!("could not start Wayland input runtime: {e}");
                    return;
                }
            };
            let mut keys = HashSet::new();
            let mut buttons = HashSet::new();
            while let Ok(event) = rx.recv() {
                let shutdown = matches!(&event, crate::input::InputEvent::Shutdown);
                let result = runtime.block_on(async {
                    use crate::input::InputEvent;
                    match event {
                        InputEvent::Move { x, y } => session.pointer(x, y).await,
                        InputEvent::Button { button, down } => {
                            let result = session.button(button, down).await;
                            if result.is_ok() {
                                if down { buttons.insert(button); } else { buttons.remove(&button); }
                            }
                            result
                        }
                        InputEvent::Scroll { dx, dy } => {
                            session.scroll(
                                f64::from(dx) * px_per_notch.max(1.0),
                                f64::from(dy) * px_per_notch.max(1.0),
                            ).await
                        }
                        InputEvent::ScrollPx { dx, dy } => session.scroll(dx, dy).await,
                        InputEvent::Key { keysym, down } => {
                            let result = session.key(keysym, down).await;
                            if result.is_ok() {
                                if down { keys.insert(keysym); } else { keys.remove(&keysym); }
                            }
                            result
                        }
                        InputEvent::Clipboard(_) => Err(
                            "clipboard synchronization is not available through this Wayland backend".into(),
                        ),
                        InputEvent::ReleaseAll | InputEvent::Shutdown => {
                            let mut first_error = None;
                            for key in keys.drain() {
                                if let Err(e) = session.key(key, false).await { first_error.get_or_insert(e); }
                            }
                            for button in buttons.drain() {
                                if let Err(e) = session.button(button, false).await { first_error.get_or_insert(e); }
                            }
                            first_error.map_or(Ok(()), Err)
                        }
                    }
                });
                if let Err(e) = result {
                    log::warn!("Wayland input event failed: {e}");
                }
                if shutdown {
                    break;
                }
            }
        })
        .expect("spawn Wayland input thread");
    tx
}

#[cfg(test)]
mod tests {
    use super::{clamp_pointer, linux_button_code, map_pointer};

    #[test]
    fn pointer_coordinates_are_clamped_to_the_selected_stream() {
        assert_eq!(clamp_pointer(-4, 900, 1280, 720), (0, 719));
        assert_eq!(clamp_pointer(1279, 0, 1280, 720), (1279, 0));
        assert_eq!(clamp_pointer(0, 0, 0, 0), (0, 0));
    }

    #[test]
    fn web_pointer_buttons_map_to_linux_input_codes() {
        assert_eq!(linux_button_code(1), Some(0x110));
        assert_eq!(linux_button_code(2), Some(0x112));
        assert_eq!(linux_button_code(3), Some(0x111));
        assert_eq!(linux_button_code(8), Some(0x116));
        assert_eq!(linux_button_code(9), Some(0x115));
        assert_eq!(linux_button_code(10), None);
    }

    #[test]
    fn pointer_coordinates_scale_from_pipewire_pixels_to_compositor_logical_size() {
        assert_eq!(map_pointer(2559, 1599, 2560, 1600, 1280, 800), (1279, 799));
        assert_eq!(map_pointer(640, 400, 2560, 1600, 1280, 800), (320, 200));
        assert_eq!(map_pointer(-1, 900, 2560, 1600, 1280, 800), (0, 450));
        assert_eq!(map_pointer(-1, 1600, 2560, 1600, 1280, 800), (0, 799));
    }
}
