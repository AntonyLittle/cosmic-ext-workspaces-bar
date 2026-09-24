// SPDX-License-Identifier: GPL-3.0-only
// Based on cosmic-workspaces-epoch, Copyright 2023 System76 <info@system76.com>

// A thread handles screencopy and other Wayland protocols, returning
// information as a subscription.

use calloop::LoopHandle;
use calloop_wayland_source::WaylandSource;
use cosmic::cctk;
use cosmic::iced::futures::channel::mpsc;
use cosmic::iced::futures::executor::block_on;
use cosmic::iced::futures::{FutureExt, SinkExt};
use cosmic::iced::{self};

use cctk::screencopy::{CaptureSource, Rect, ScreencopyState};
use cctk::sctk::registry::{ProvidesRegistryState, RegistryState};
use cctk::sctk::seat::{SeatHandler, SeatState};
use cctk::sctk::shm::{Shm, ShmHandler};
use cctk::sctk::{self};
use cctk::wayland_client::globals::registry_queue_init;
use cctk::wayland_client::protocol::{wl_output, wl_seat};
use cctk::wayland_client::{Connection, QueueHandle};
use cctk::workspace::WorkspaceState;

use crate::config::Edge;

use std::cell::RefCell;
use std::collections::HashMap;
use std::hash::Hash;
use std::sync::Arc;
use std::thread;
use std::time::Duration;

mod buffer;
use buffer::Buffer;
mod capture;
use capture::Capture;
mod screencopy;
use screencopy::{ScreencopySession, SessionData};
mod workspace;

use super::{Cmd, Event};

/// Minimum delay between capture requests per workspace, to bound CPU use
const CAPTURE_INTERVAL: Duration = Duration::from_millis(100);
/// Maximum backoff when a workspace is idle
const MAX_CAPTURE_INTERVAL: Duration = Duration::from_millis(2000);
/// Per-channel tolerance when comparing frames; compositor blur behind
/// translucent surfaces produces per-frame dither noise
const PIXEL_TOLERANCE: u8 = 6;

fn rows_similar(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).all(|(x, y)| x.abs_diff(*y) <= PIXEL_TOLERANCE)
}

pub struct BarFilter {
    pub edge: Edge,
    pub size: u32,
    pub outputs: Vec<(wl_output::WlOutput, (i32, i32))>,
    pub paused: bool,
    pub clips: Vec<(wl_output::WlOutput, [u32; 4])>,
}

pub fn subscription(conn: Connection) -> iced::Subscription<Event> {
    #[derive(Clone)]
    struct WaylandSubscription(Connection);
    impl Hash for WaylandSubscription {
        fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
            self.0.backend().display_id().hash(state);
        }
    }
    iced::Subscription::run_with(
        WaylandSubscription(conn.clone()),
        |WaylandSubscription(conn)| {
            let conn = conn.clone();
            async { start(conn) }.flatten_stream()
        },
    )
}

pub struct AppData {
    qh: QueueHandle<Self>,
    loop_handle: LoopHandle<'static, AppData>,
    registry_state: RegistryState,
    workspace_state: WorkspaceState,
    screencopy_state: ScreencopyState,
    seat_state: SeatState,
    shm_state: Shm,
    sender: mpsc::Sender<Event>,
    captures: RefCell<HashMap<CaptureSource, Arc<Capture>>>,
    bar_filter: Option<BarFilter>,
}

impl AppData {
    fn captures_paused(&self) -> bool {
        self.bar_filter.as_ref().is_some_and(|f| f.paused)
    }

    // Bar strip rectangle in buffer coordinates for the workspace's output
    fn bar_strip(&self, source: &CaptureSource, buffer_size: (u32, u32)) -> Option<Rect> {
        let filter = self.bar_filter.as_ref()?;
        let CaptureSource::Workspace(ws) = source else {
            return None;
        };
        let (lw, lh) = self
            .workspace_state
            .workspace_groups()
            .find(|g| g.workspaces.iter().any(|w| w == ws))
            .and_then(|g| {
                g.outputs.iter().find_map(|o| {
                    filter
                        .outputs
                        .iter()
                        .find(|(fo, _)| fo == o)
                        .map(|(_, s)| *s)
                })
            })?;
        if lw <= 0 || lh <= 0 {
            return None;
        }
        let bw = buffer_size.0 as f32;
        let bh = buffer_size.1 as f32;
        let strip_w = (filter.size as f32 * (bw / lw as f32)).ceil() as i32 + 1;
        let strip_h = (filter.size as f32 * (bh / lh as f32)).ceil() as i32 + 1;
        Some(match filter.edge {
            Edge::Top => Rect {
                x: 0,
                y: 0,
                width: bw as i32,
                height: strip_h,
            },
            Edge::Bottom => Rect {
                x: 0,
                y: bh as i32 - strip_h,
                width: bw as i32,
                height: strip_h,
            },
            Edge::Left => Rect {
                x: 0,
                y: 0,
                width: strip_w,
                height: bh as i32,
            },
            Edge::Right => Rect {
                x: bw as i32 - strip_w,
                y: 0,
                width: strip_w,
                height: bh as i32,
            },
        })
    }

    // Buffer-space crop thickness per edge for the workspace's output, and
    // whether the crop fully covers the bar's own strip
    pub(crate) fn clip_for(
        &self,
        source: &CaptureSource,
        buffer_size: (u32, u32),
    ) -> Option<([u32; 4], bool)> {
        let filter = self.bar_filter.as_ref()?;
        let CaptureSource::Workspace(ws) = source else {
            return None;
        };
        let group = self
            .workspace_state
            .workspace_groups()
            .find(|g| g.workspaces.iter().any(|w| w == ws))?;
        let (output, (lw, lh)) = group.outputs.iter().find_map(|o| {
            filter
                .outputs
                .iter()
                .find(|(fo, _)| fo == o)
                .map(|(fo, s)| (fo.clone(), *s))
        })?;
        if lw <= 0 || lh <= 0 {
            return None;
        }
        let clips = filter
            .clips
            .iter()
            .find(|(o, _)| *o == output)
            .map(|(_, c)| *c)?;
        if clips == [0; 4] {
            return None;
        }
        let sx = buffer_size.0 as f32 / lw as f32;
        let sy = buffer_size.1 as f32 / lh as f32;
        let buf = [
            (clips[0] as f32 * sy).ceil() as u32,
            (clips[1] as f32 * sy).ceil() as u32,
            (clips[2] as f32 * sx).ceil() as u32,
            (clips[3] as f32 * sx).ceil() as u32,
        ];
        let own_covered = match filter.edge {
            Edge::Top => clips[0] >= filter.size,
            Edge::Bottom => clips[1] >= filter.size,
            Edge::Left => clips[2] >= filter.size,
            Edge::Right => clips[3] >= filter.size,
        };
        Some((buf, own_covered))
    }

    // True if the two downscaled frames are identical outside the bar strip
    pub(crate) fn small_unchanged(
        &self,
        source: &CaptureSource,
        size: (u32, u32),
        prev: &[u8],
        cur: &[u8],
        own_clipped: bool,
    ) -> bool {
        if prev.len() != cur.len() {
            return false;
        }
        // Cropped frames no longer contain the bar strip
        if own_clipped {
            return rows_similar(prev, cur);
        }
        let (w, h) = size;
        let stride = w as usize * 4;
        let Some(strip) = self.bar_strip(source, size) else {
            return rows_similar(prev, cur);
        };
        let sx0 = (strip.x.max(0) as usize * 4).min(stride);
        let sx1 = (((strip.x + strip.width).clamp(0, w as i32)) as usize * 4).min(stride);
        let sy0 = strip.y.max(0) as usize;
        let sy1 = ((strip.y + strip.height).clamp(0, h as i32)) as usize;
        for y in 0..h as usize {
            let row_a = &prev[y * stride..(y + 1) * stride];
            let row_b = &cur[y * stride..(y + 1) * stride];
            if y < sy0 || y >= sy1 {
                if !rows_similar(row_a, row_b) {
                    return false;
                }
            } else if !rows_similar(&row_a[..sx0], &row_b[..sx0])
                || !rows_similar(&row_a[sx1..], &row_b[sx1..])
            {
                return false;
            }
        }
        true
    }

    fn send_event(&mut self, event: Event) {
        let _ = block_on(self.sender.send(event));
    }

    // Handle message from main thread
    fn handle_cmd(&mut self, cmd: Cmd) {
        match cmd {
            Cmd::ActivateWorkspace(workspace_handle) => {
                if let Ok(workspace_manager) = self.workspace_state.workspace_manager().get() {
                    workspace_handle.activate();
                    workspace_manager.commit();
                }
            }
            Cmd::RenameWorkspace(workspace_handle, name) => {
                if let Some(workspace) = self.workspace_state.workspace_info(&workspace_handle)
                    && let Some(cosmic_handle) = workspace.cosmic_handle.as_ref()
                {
                    cosmic_handle.rename(name);
                    if let Ok(workspace_manager) = self.workspace_state.workspace_manager().get() {
                        workspace_manager.commit();
                    }
                }
            }
            Cmd::BarFilter {
                edge,
                size,
                outputs,
                paused,
                clips,
            } => {
                let was_paused = self.captures_paused();
                self.bar_filter = Some(BarFilter {
                    edge,
                    size,
                    outputs,
                    paused,
                    clips,
                });
                if paused && !was_paused {
                    for capture in self.captures.borrow().values() {
                        capture.stop();
                    }
                } else if !paused && was_paused {
                    for capture in self.captures.borrow().values() {
                        capture.start(&self.screencopy_state, &self.qh);
                    }
                }
            }
        }
    }

    fn add_capture_source(&self, source: CaptureSource) {
        let paused = self.captures_paused();
        self.captures
            .borrow_mut()
            .entry(source.clone())
            .or_insert_with(|| {
                let capture = Capture::new(source);
                if !paused {
                    capture.start(&self.screencopy_state, &self.qh);
                }
                capture
            });
    }

    fn retain_capture_sources(&self, live: &[CaptureSource]) {
        self.captures.borrow_mut().retain(|source, capture| {
            let keep = live.contains(source);
            if !keep {
                capture.stop();
            }
            keep
        });
    }
}

impl ProvidesRegistryState for AppData {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry_state
    }

    sctk::registry_handlers!(SeatState);
}

impl SeatHandler for AppData {
    fn seat_state(&mut self) -> &mut SeatState {
        &mut self.seat_state
    }

    fn new_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _seat: wl_seat::WlSeat) {}

    fn remove_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _seat: wl_seat::WlSeat) {}

    fn new_capability(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: wl_seat::WlSeat,
        _: sctk::seat::Capability,
    ) {
    }

    fn remove_capability(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: wl_seat::WlSeat,
        _: sctk::seat::Capability,
    ) {
    }
}

impl ShmHandler for AppData {
    fn shm_state(&mut self) -> &mut Shm {
        &mut self.shm_state
    }
}

fn start(conn: Connection) -> mpsc::Receiver<Event> {
    let (sender, receiver) = mpsc::channel(20);

    let (globals, event_queue) = registry_queue_init(&conn).unwrap();
    let qh = event_queue.handle();

    thread::spawn(move || {
        let mut event_loop = calloop::EventLoop::try_new().unwrap();

        let registry_state = RegistryState::new(&globals);
        let mut app_data = AppData {
            qh: qh.clone(),
            loop_handle: event_loop.handle(),
            workspace_state: WorkspaceState::new(&registry_state, &qh),
            screencopy_state: ScreencopyState::new(&globals, &qh),
            registry_state,
            seat_state: SeatState::new(&globals, &qh),
            shm_state: Shm::bind(&globals, &qh).unwrap(),
            sender,
            captures: RefCell::new(HashMap::new()),
            bar_filter: None,
        };

        let (cmd_sender, cmd_channel) = calloop::channel::channel();
        app_data.send_event(Event::CmdSender(cmd_sender));

        WaylandSource::new(conn, event_queue)
            .insert(event_loop.handle())
            .unwrap();
        event_loop
            .handle()
            .insert_source(cmd_channel, |event, _, app_data| {
                if let calloop::channel::Event::Msg(msg) = event {
                    app_data.handle_cmd(msg)
                }
            })
            .unwrap();

        loop {
            event_loop.dispatch(None, &mut app_data).unwrap();
        }
    });

    receiver
}

// Don't bind outputs; use `WlOutput` instances from iced-sctk
sctk::delegate_registry!(AppData);
sctk::delegate_seat!(AppData);
sctk::delegate_shm!(AppData);
