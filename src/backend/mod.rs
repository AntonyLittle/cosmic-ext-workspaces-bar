// SPDX-License-Identifier: GPL-3.0-only
// Based on cosmic-workspaces-epoch, Copyright 2023 System76 <info@system76.com>

//! Backend getting workspace information and live previews from cosmic-comp,
//! and sending workspace activation commands.

use cosmic::cctk::wayland_client::protocol::wl_output;
use std::collections::HashSet;

mod wayland;
pub mod media;
pub mod power;
pub use cosmic::cctk::wayland_protocols::ext::workspace::v1::client::ext_workspace_handle_v1::ExtWorkspaceHandleV1;
pub use cosmic::cctk::workspace::Workspace;
pub use wayland::subscription;

#[derive(Clone, Debug)]
pub struct CaptureImage {
    pub width: u32,
    pub height: u32,
    pub image: cosmic::widget::image::Handle,
}

#[derive(Clone, Debug)]
pub enum Event {
    CmdSender(calloop::channel::Sender<Cmd>),
    Workspaces(Vec<(HashSet<wl_output::WlOutput>, Workspace)>),
    WorkspaceCapture(ExtWorkspaceHandleV1, CaptureImage),
}

#[derive(Debug)]
pub enum Cmd {
    ActivateWorkspace(ExtWorkspaceHandleV1),
    RenameWorkspace(ExtWorkspaceHandleV1, String),
    /// Switch to the workspace containing a toplevel matching this app id
    /// (fuzzy substring match), and activate the toplevel itself if possible
    ActivateToplevelByAppId(String),
    /// Tear down and recreate all capture sessions, and reset their
    /// throttling - used after the session unlocks or the system resumes
    /// from sleep, since captures taken while locked show the lock screen
    /// overlay, and suspend can otherwise leave sessions stale
    RefreshCaptures,
    /// Geometry of the bar and outputs, for filtering self-caused damage,
    /// plus a pause flag used while the settings dialog is open (its redraws
    /// would otherwise feed a capture -> repaint -> capture loop)
    BarFilter {
        edge: crate::config::Edge,
        size: u32,
        outputs: Vec<(wl_output::WlOutput, (i32, i32))>,
        paused: bool,
        /// Downscale target for preview thumbnails, in pixels
        preview_px: u32,
        /// Logical clip thickness per edge (top, bottom, left, right) to
        /// crop out of thumbnails, per output
        clips: Vec<(wl_output::WlOutput, [u32; 4])>,
    },
}
