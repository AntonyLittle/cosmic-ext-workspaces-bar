// SPDX-License-Identifier: GPL-3.0-only
// Based on cosmic-workspaces-epoch, Copyright 2023 System76 <info@system76.com>

use cosmic::cctk;

use cctk::screencopy::CaptureSource;
use cctk::workspace::{WorkspaceHandler, WorkspaceState};

use super::{AppData, Event};

impl WorkspaceHandler for AppData {
    fn workspace_state(&mut self) -> &mut WorkspaceState {
        &mut self.workspace_state
    }

    fn done(&mut self) {
        let mut workspaces = Vec::new();
        let mut sources = Vec::new();

        for group in self.workspace_state.workspace_groups() {
            for workspace_handle in &group.workspaces {
                if let Some(workspace) = self.workspace_state.workspace_info(workspace_handle) {
                    workspaces.push((group.outputs.iter().cloned().collect(), workspace.clone()));
                    sources.push(CaptureSource::Workspace(workspace_handle.clone()));
                }
            }
        }

        self.retain_capture_sources(&sources);
        for source in sources {
            self.add_capture_source(source);
        }

        // Workspace switches should refresh previews promptly
        self.reset_capture_intervals();

        self.send_event(Event::Workspaces(workspaces));
    }
}

cctk::delegate_workspace!(AppData);
