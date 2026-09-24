// SPDX-License-Identifier: GPL-3.0-only

use cosmic::cctk;

use cctk::cosmic_protocols::toplevel_management::v1::client::zcosmic_toplevel_manager_v1::ZcosmicToplelevelManagementCapabilitiesV1;
use cctk::toplevel_info::{ToplevelInfoHandler, ToplevelInfoState};
use cctk::toplevel_management::{ToplevelManagerHandler, ToplevelManagerState};
use cctk::wayland_client::{Connection, QueueHandle, WEnum};
use cctk::wayland_protocols::ext::foreign_toplevel_list::v1::client::ext_foreign_toplevel_handle_v1::ExtForeignToplevelHandleV1;

use super::AppData;

impl ToplevelInfoHandler for AppData {
    fn toplevel_info_state(&mut self) -> &mut ToplevelInfoState {
        &mut self.toplevel_info_state
    }

    fn new_toplevel(&mut self, _conn: &Connection, _qh: &QueueHandle<Self>, _toplevel: &ExtForeignToplevelHandleV1) {}

    fn update_toplevel(&mut self, _conn: &Connection, _qh: &QueueHandle<Self>, _toplevel: &ExtForeignToplevelHandleV1) {}

    fn toplevel_closed(&mut self, _conn: &Connection, _qh: &QueueHandle<Self>, _toplevel: &ExtForeignToplevelHandleV1) {}
}

impl ToplevelManagerHandler for AppData {
    fn toplevel_manager_state(&mut self) -> &mut ToplevelManagerState {
        self.toplevel_manager_state
            .as_mut()
            .expect("capabilities event implies the manager state exists")
    }

    fn capabilities(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _capabilities: Vec<WEnum<ZcosmicToplelevelManagementCapabilitiesV1>>,
    ) {
    }
}

cctk::delegate_toplevel_info!(AppData);
cctk::delegate_toplevel_manager!(AppData);
