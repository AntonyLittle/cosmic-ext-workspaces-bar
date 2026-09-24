// SPDX-License-Identifier: GPL-3.0-only
// Based on cosmic-workspaces-epoch, Copyright 2023 System76 <info@system76.com>
// Simplified: shm-only, previews delivered as iced image handles, and capture
// rate throttled with a calloop timer since the bar is always visible.

use calloop::timer::{TimeoutAction, Timer};
use cosmic::cctk;
use cosmic::iced::core::Bytes;

use cctk::screencopy::{
    CaptureFrame, CaptureOptions, CaptureSession, CaptureSource, FailureReason, Formats, Frame,
    ScreencopyFrameData, ScreencopyFrameDataExt, ScreencopyHandler, ScreencopySessionData,
    ScreencopySessionDataExt, ScreencopyState,
};
use cctk::wayland_client::protocol::wl_buffer;
use cctk::wayland_client::{Connection, QueueHandle, WEnum};

use std::array;
use std::sync::{Arc, Weak};

use super::dmabuf::DmabufBuffer;
use super::{AppData, Buffer, CAPTURE_INTERVAL, Capture, Event, MAX_CAPTURE_INTERVAL};
use crate::backend::CaptureImage;

// We copy out of the buffer synchronously in `ready`, so one is enough
const BUFFER_COUNT: usize = 1;

// Either a plain shm buffer, or a GPU-allocated dmabuf one (stage 1: still
// read back on the CPU, but this is the prerequisite for a GPU downscale in
// a later stage). Falls back to `Shm` whenever dmabuf isn't usable.
enum CaptureBuffer {
    Shm(Buffer),
    Dmabuf(DmabufBuffer),
}

impl CaptureBuffer {
    fn wl_buffer(&self) -> &wl_buffer::WlBuffer {
        match self {
            CaptureBuffer::Shm(b) => &b.buffer,
            CaptureBuffer::Dmabuf(b) => &b.buffer,
        }
    }

    fn damage(&self) -> &[cctk::screencopy::Rect] {
        match self {
            CaptureBuffer::Shm(b) => &b.buffer_damage,
            CaptureBuffer::Dmabuf(b) => &b.buffer_damage,
        }
    }

    fn clear_damage(&mut self) {
        match self {
            CaptureBuffer::Shm(b) => b.buffer_damage.clear(),
            CaptureBuffer::Dmabuf(b) => b.buffer_damage.clear(),
        }
    }

    fn size(&self) -> (u32, u32) {
        match self {
            CaptureBuffer::Shm(b) => b.size,
            CaptureBuffer::Dmabuf(b) => b.size,
        }
    }
}

impl AppData {
    // Prefer a dmabuf-backed buffer when the compositor advertises one for
    // this session; falls back to shm on any allocation failure
    fn create_capture_buffer(&mut self, formats: &Formats) -> CaptureBuffer {
        if let Some(device) = formats.dmabuf_device
            && !formats.dmabuf_formats.is_empty()
            && let Some(buf) =
                self.create_dmabuf_buffer(device, &formats.dmabuf_formats, formats.buffer_size)
        {
            return CaptureBuffer::Dmabuf(buf);
        }
        CaptureBuffer::Shm(self.create_buffer(formats))
    }
}

pub struct ScreencopySession {
    formats: Option<Formats>,
    buffers: Option<[CaptureBuffer; BUFFER_COUNT]>,
    session: CaptureSession,
    // adaptive throttle: grows while the workspace is idle
    interval: std::time::Duration,
    // previous downscaled frame, for change detection
    prev_small: Option<Bytes>,
    // capture requested but not yet delivered
    in_flight: bool,
    // pending re-capture timer, so workspace switches can preempt it
    timer_token: Option<calloop::RegistrationToken>,
}

impl ScreencopySession {
    pub fn new(
        capture: &Arc<Capture>,
        screencopy_state: &ScreencopyState,
        qh: &QueueHandle<AppData>,
    ) -> Self {
        let udata = SessionData {
            session_data: Default::default(),
            capture: Arc::downgrade(capture),
        };

        let session = screencopy_state
            .capturer()
            .create_session(&capture.source, CaptureOptions::empty(), qh, udata)
            .unwrap();

        Self {
            formats: None,
            buffers: None,
            session,
            interval: CAPTURE_INTERVAL,
            prev_small: None,
            in_flight: false,
            timer_token: None,
        }
    }

    fn attach_buffer_and_commit(
        &mut self,
        capture: &Arc<Capture>,
        conn: &Connection,
        qh: &QueueHandle<AppData>,
    ) {
        let Some(back) = self.buffers.as_ref().map(|x| &x[0]) else {
            return;
        };

        self.in_flight = true;
        self.session.capture(
            back.wl_buffer(),
            back.damage(),
            qh,
            FrameData {
                frame_data: Default::default(),
                capture: Arc::downgrade(capture),
            },
        );
        conn.flush().unwrap();
    }
}

pub struct SessionData {
    session_data: ScreencopySessionData,
    // Weak reference so session can be destroyed when all strong references
    // are dropped.
    pub capture: Weak<Capture>,
}

impl ScreencopySessionDataExt for SessionData {
    fn screencopy_session_data(&self) -> &ScreencopySessionData {
        &self.session_data
    }
}

struct FrameData {
    frame_data: ScreencopyFrameData,
    capture: Weak<Capture>,
}

impl ScreencopyFrameDataExt for FrameData {
    fn screencopy_frame_data(&self) -> &ScreencopyFrameData {
        &self.frame_data
    }
}

impl ScreencopyHandler for AppData {
    fn screencopy_state(&mut self) -> &mut ScreencopyState {
        &mut self.screencopy_state
    }

    fn init_done(
        &mut self,
        conn: &Connection,
        _qh: &QueueHandle<Self>,
        session: &CaptureSession,
        formats: &Formats,
    ) {
        let Some(capture) = Capture::for_session(session) else {
            return;
        };
        let mut session = capture.session.lock().unwrap();
        let Some(session) = session.as_mut() else {
            return;
        };

        session.formats = Some(formats.clone());

        // Create new buffer if none, then start capturing
        if session.buffers.is_none() {
            session.buffers = Some(array::from_fn(|_| self.create_capture_buffer(formats)));
            session.attach_buffer_and_commit(&capture, conn, &self.qh);
        }
    }

    fn ready(
        &mut self,
        conn: &Connection,
        qh: &QueueHandle<Self>,
        capture_frame: &CaptureFrame,
        _frame: Frame,
    ) {
        let capture = &capture_frame.data::<FrameData>().unwrap().capture;
        let Some(capture) = capture.upgrade() else {
            return;
        };
        let mut session_guard = capture.session.lock().unwrap();
        let Some(session) = session_guard.as_mut() else {
            return;
        };
        session.in_flight = false;

        if session.buffers.is_none() {
            log::error!("No capture buffers?");
            return;
        }

        // Single reused buffer: after this copy it matches compositor state
        session.buffers.as_mut().unwrap()[0].clear_damage();

        let bufs = session.buffers.as_ref().unwrap();
        let front = &bufs[0];
        // Downscale to preview resolution; all further work is on the small image
        let target = self.bar_filter.as_ref().map_or(512, |f| f.preview_px);
        let (w, h) = front.size();
        let dmabuf_device = session.formats.as_ref().and_then(|f| f.dmabuf_device);
        let downscaled = match front {
            CaptureBuffer::Shm(b) => Some(downscale(w, h, &b.mmap[..], w as usize * 4, target)),
            CaptureBuffer::Dmabuf(b) => {
                let gpu_result = dmabuf_device
                    .and_then(|dev| self.gpu_downscaler(dev))
                    .and_then(|gd| gd.downscale(b, target));
                gpu_result.or_else(|| match b.with_pixels(|data, stride| {
                    downscale(w, h, data, stride as usize, target)
                }) {
                    Ok(result) => Some(result),
                    Err(err) => {
                        log::warn!("dmabuf CPU readback fallback failed: {err}");
                        None
                    }
                })
            }
        };
        let Some((sw, sh, small)) = downscaled else {
            return;
        };
        // Crop out the configured reserved strips (own bar / all bars)
        let clip = self.clip_for(&capture.source, (sw, sh));
        let (sw, sh, small) = match clip {
            Some((edges, _)) => crop(sw, sh, small, edges),
            None => (sw, sh, small),
        };
        let own_clipped = clip.is_some_and(|(_, own)| own);
        // Skip frames identical outside the bar strip: they are our own
        // repaints and forwarding them would loop capture -> repaint -> capture
        let small = Bytes::from(small);
        let skip = session.prev_small.as_ref().is_some_and(|prev| {
            self.small_unchanged(&capture.source, (sw, sh), prev, &small, own_clipped)
        });
        let image = (!skip).then(|| CaptureImage {
            width: sw,
            height: sh,
            image: cosmic::widget::image::Handle::from_rgba(sw, sh, small.clone()),
        });
        session.prev_small = Some(small);
        // Idle workspaces back off; activity resets to the base rate
        session.interval = if skip {
            (session.interval * 2).min(MAX_CAPTURE_INTERVAL)
        } else {
            CAPTURE_INTERVAL
        };
        let interval = session.interval;
        drop(session_guard);

        // Request the next frame after the adaptive throttle delay
        let capture_clone = capture.clone();
        let conn = conn.clone();
        let qh = qh.clone();
        let token = self
            .loop_handle
            .insert_source(Timer::from_duration(interval), move |_, _, _| {
                let mut session = capture_clone.session.lock().unwrap();
                if let Some(session) = session.as_mut() {
                    session.timer_token = None;
                    session.attach_buffer_and_commit(&capture_clone, &conn, &qh);
                }
                TimeoutAction::Drop
            })
            .unwrap();
        if let Some(session) = capture.session.lock().unwrap().as_mut() {
            session.timer_token = Some(token);
        }

        if let Some(image) = image {
            match &capture.source {
                CaptureSource::Workspace(workspace) => {
                    self.send_event(Event::WorkspaceCapture(workspace.clone(), image));
                }
                _ => unreachable!(),
            };
        }
    }

    fn failed(
        &mut self,
        conn: &Connection,
        _qh: &QueueHandle<Self>,
        capture_frame: &CaptureFrame,
        reason: WEnum<FailureReason>,
    ) {
        let capture = &capture_frame.data::<FrameData>().unwrap().capture;
        let Some(capture) = capture.upgrade() else {
            return;
        };
        if reason == WEnum::Value(FailureReason::BufferConstraints) {
            // Re-allocate buffers, then trigger another capture
            log::info!("buffer constraint failure; re-allocating");
            let mut session = capture.session.lock().unwrap();
            let Some(session) = session.as_mut() else {
                return;
            };
            if let Some(formats) = &session.formats {
                let formats = formats.clone();
                session.buffers = Some(array::from_fn(|_| self.create_capture_buffer(&formats)));
            }
            session.attach_buffer_and_commit(&capture, conn, &self.qh);
        } else {
            if reason == WEnum::Value(FailureReason::Stopped) {
                log::info!("Screencopy frame capture stopped");
            } else {
                log::error!("Screencopy failed: {:?}", reason);
            }
            capture.stop();
        }
    }

    fn stopped(&mut self, _conn: &Connection, _qh: &QueueHandle<Self>, session: &CaptureSession) {
        if let Some(capture) = Capture::for_session(session) {
            capture.stop();
        }
    }
}

impl AppData {
    // Reset throttles and re-capture promptly; used on workspace changes
    pub(crate) fn reset_capture_intervals(&self) {
        for capture in self.captures.borrow().values() {
            let mut guard = capture.session.lock().unwrap();
            if let Some(session) = guard.as_mut() {
                session.interval = CAPTURE_INTERVAL;
                if !session.in_flight
                    && let Some(token) = session.timer_token.take()
                {
                    self.loop_handle.remove(token);
                    session.attach_buffer_and_commit(capture, &self.conn, &self.qh);
                }
            }
        }
    }
}

cctk::delegate_screencopy!(AppData);

/// Nearest-neighbor downscale to at most `target` px wide. `stride` is the
/// source row stride in bytes, which may exceed `w * 4` (dmabuf rows are
/// often padded to an alignment boundary; shm rows are tightly packed)
fn downscale(w: u32, h: u32, src: &[u8], stride: usize, target: u32) -> (u32, u32, Vec<u8>) {
    let target = target.max(64).min(w.max(1));
    let scale = w as f32 / target as f32;
    let sw = target;
    let sh = ((h as f32 / scale).round() as u32).max(1);
    let mut out = Vec::with_capacity((sw * sh * 4) as usize);
    for y in 0..sh {
        let sy = (((y as f32 + 0.5) * scale) as usize).min(h as usize - 1);
        let row = &src[sy * stride..sy * stride + w as usize * 4];
        for x in 0..sw {
            let sx = ((((x as f32 + 0.5) * scale) as usize).min(w as usize - 1)) * 4;
            out.extend_from_slice(&row[sx..sx + 4]);
        }
    }
    (sw, sh, out)
}

/// Remove crop thicknesses (top, bottom, left, right) from an RGBA image
fn crop(w: u32, h: u32, data: Vec<u8>, edges: [u32; 4]) -> (u32, u32, Vec<u8>) {
    let [top, bottom, left, right] = edges.map(|e| e as usize);
    let (w, h) = (w as usize, h as usize);
    if top + bottom >= h || left + right >= w {
        return (w as u32, h as u32, data);
    }
    let stride = w * 4;
    let new_w = w - left - right;
    let new_h = h - top - bottom;
    let mut out = Vec::with_capacity(new_w * new_h * 4);
    for y in top..h - bottom {
        let row = &data[y * stride..(y + 1) * stride];
        out.extend_from_slice(&row[left * 4..(w - right) * 4]);
    }
    (new_w as u32, new_h as u32, out)
}
