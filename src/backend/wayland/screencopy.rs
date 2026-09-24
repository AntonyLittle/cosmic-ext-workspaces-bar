// SPDX-License-Identifier: GPL-3.0-only
// Based on cosmic-workspaces-epoch, Copyright 2023 System76 <info@system76.com>
// Simplified: shm-only, previews delivered as iced image handles, and capture
// rate throttled with a calloop timer since the bar is always visible.

use calloop::timer::{TimeoutAction, Timer};
use cosmic::cctk;

use cctk::screencopy::{
    CaptureFrame, CaptureOptions, CaptureSession, CaptureSource, FailureReason, Formats, Frame,
    ScreencopyFrameData, ScreencopyFrameDataExt, ScreencopyHandler, ScreencopySessionData,
    ScreencopySessionDataExt, ScreencopyState,
};
use cctk::wayland_client::{Connection, QueueHandle, WEnum};

use std::array;
use std::sync::{Arc, Weak};

use super::{AppData, Buffer, CAPTURE_INTERVAL, Capture, Event, MAX_CAPTURE_INTERVAL};
use crate::backend::CaptureImage;

// Number of buffers to swap between
const BUFFER_COUNT: usize = 2;

pub struct ScreencopySession {
    formats: Option<Formats>,
    // swapchain buffers
    buffers: Option<[Buffer; BUFFER_COUNT]>,
    session: CaptureSession,
    // adaptive throttle: grows while the workspace is idle
    interval: std::time::Duration,
    // previous downscaled frame, for change detection
    prev_small: Option<Vec<u8>>,
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
        }
    }

    fn attach_buffer_and_commit(
        &mut self,
        capture: &Arc<Capture>,
        conn: &Connection,
        qh: &QueueHandle<AppData>,
    ) {
        let Some(back) = self.buffers.as_ref().map(|x| &x[1]) else {
            return;
        };

        self.session.capture(
            &back.buffer,
            &back.buffer_damage,
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
            session.buffers = Some(array::from_fn(|_| self.create_buffer(formats)));
            session.attach_buffer_and_commit(&capture, conn, &self.qh);
        }
    }

    fn ready(
        &mut self,
        conn: &Connection,
        qh: &QueueHandle<Self>,
        capture_frame: &CaptureFrame,
        frame: Frame,
    ) {
        let capture = &capture_frame.data::<FrameData>().unwrap().capture;
        let Some(capture) = capture.upgrade() else {
            return;
        };
        let mut session_guard = capture.session.lock().unwrap();
        let Some(session) = session_guard.as_mut() else {
            return;
        };

        if session.buffers.is_none() {
            log::error!("No capture buffers?");
            return;
        }

        // swap buffers
        session.buffers.as_mut().unwrap().rotate_left(1);

        // Clear `buffer_damage` for front buffer; accumulate for other buffers.
        session.buffers.as_mut().unwrap()[0].buffer_damage.clear();
        for buffer in &mut session.buffers.as_mut().unwrap()[1..] {
            buffer.buffer_damage.extend_from_slice(&frame.damage);
        }

        let bufs = session.buffers.as_ref().unwrap();
        let front = &bufs[0];
        // Downscale to preview resolution; all further work is on the small image
        let (sw, sh, small) = downscale(front);
        // Crop out the configured reserved strips (own bar / all bars)
        let clip = self.clip_for(&capture.source, (sw, sh));
        let (sw, sh, small) = match clip {
            Some((edges, _)) => crop(sw, sh, small, edges),
            None => (sw, sh, small),
        };
        let own_clipped = clip.is_some_and(|(_, own)| own);
        // Skip frames identical outside the bar strip: they are our own
        // repaints and forwarding them would loop capture -> repaint -> capture
        let skip = session
            .prev_small
            .as_ref()
            .is_some_and(|prev| {
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
        self.loop_handle
            .insert_source(Timer::from_duration(interval), move |_, _, _| {
                let mut session = capture_clone.session.lock().unwrap();
                if let Some(session) = session.as_mut() {
                    session.attach_buffer_and_commit(&capture_clone, &conn, &qh);
                }
                TimeoutAction::Drop
            })
            .unwrap();

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
                session.buffers = Some(array::from_fn(|_| self.create_buffer(&formats)));
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

cctk::delegate_screencopy!(AppData);

/// Nearest-neighbor downscale to at most 512px wide
fn downscale(buf: &Buffer) -> (u32, u32, Vec<u8>) {
    let (w, h) = buf.size;
    let target = 512u32.min(w.max(1));
    let scale = w as f32 / target as f32;
    let sw = target;
    let sh = ((h as f32 / scale).round() as u32).max(1);
    let src = &buf.mmap[..];
    let stride = w as usize * 4;
    let mut out = Vec::with_capacity((sw * sh * 4) as usize);
    for y in 0..sh {
        let sy = (((y as f32 + 0.5) * scale) as usize).min(h as usize - 1);
        let row = &src[sy * stride..(sy + 1) * stride];
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
