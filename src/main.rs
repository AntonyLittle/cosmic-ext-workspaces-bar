// SPDX-License-Identifier: GPL-3.0-only
// Based in part on cosmic-workspaces-epoch, Copyright 2023 System76 <info@system76.com>

mod backend;
mod config;
mod settings;
mod utils;
mod view;

use cosmic::app::{Application, CosmicFlags};
use cosmic::cctk;
use cosmic::iced::event::wayland::{
    Event as WaylandEvent, LayerEvent, OutputEvent, OverlapNotifyEvent, PopupEvent,
};
use cosmic::iced::mouse::ScrollDelta;
use cosmic::iced::platform_specific::shell::commands::layer_surface::{
    destroy_layer_surface, get_layer_surface, set_exclusive_zone, set_margin, set_size,
};
use cosmic::iced::platform_specific::shell::commands::overlap_notify::overlap_notify;
use cosmic::iced::platform_specific::shell::commands::popup::{destroy_popup, get_popup};
use cosmic::iced::runtime::platform_specific::wayland::layer_surface::{
    IcedOutput, SctkLayerSurfaceSettings,
};
use cosmic::iced::runtime::platform_specific::wayland::popup::{
    SctkPopupSettings, SctkPositioner,
};
use cosmic::iced::window::Id as SurfaceId;
use cosmic::iced::{self, Subscription, Task};
use cosmic::scroll::DiscreteScrollState;

use cctk::sctk::reexports::protocols::xdg::shell::client::xdg_positioner;
use cctk::sctk::shell::wlr_layer::{Anchor, KeyboardInteractivity, Layer};
use cctk::wayland_client::protocol::wl_output;
use cctk::wayland_client::{Connection, Proxy};
use cctk::wayland_protocols::ext::workspace::v1::client::ext_workspace_handle_v1;

use backend::ExtWorkspaceHandleV1;

use std::collections::{HashMap, HashSet};
use std::mem;

/// Sliver left on screen for hover reveal while auto-hidden
const AUTOHIDE_HANDLE: u32 = 4;
/// Delay without pointer focus before the bar hides
const AUTOHIDE_WAIT: std::time::Duration = std::time::Duration::from_millis(1000);

// Negative margin on the anchored edge slides the bar off-screen
fn hidden_margin(edge: config::Edge, size: u32) -> (i32, i32, i32, i32) {
    let offset = -(size.saturating_sub(AUTOHIDE_HANDLE) as i32);
    match edge {
        config::Edge::Top => (offset, 0, 0, 0),
        config::Edge::Bottom => (0, 0, offset, 0),
        config::Edge::Left => (0, 0, 0, offset),
        config::Edge::Right => (0, offset, 0, 0),
    }
}

#[derive(Clone, Debug)]
pub enum Msg {
    WaylandEvent(WaylandEvent),
    Wayland(backend::Event),
    Config(config::Config),
    PanelTheme(config::PanelTheme),
    ActivateWorkspace(ExtWorkspaceHandleV1),
    OnScroll(wl_output::WlOutput, ScrollDelta),
    BarEnter(SurfaceId),
    BarExit(SurfaceId),
    BarMove(SurfaceId, iced::Point),
    HideTimeout(SurfaceId, u64),
    OpenContextMenu(SurfaceId, Option<(ExtWorkspaceHandleV1, String)>),
    MenuRename,
    MenuSettings,
    RenameInput(String),
    RenameSubmit,
    RenameCancel,
    OpenSettings,
    Settings(settings::SettingsMsg),
    CloseSettings,
    CloseWindow(SurfaceId),
    Media(backend::media::Event),
    MediaControl(backend::media::Control),
    Ignore,
}

#[derive(Clone, Debug)]
pub struct Workspace {
    pub info: backend::Workspace,
    pub img: Option<backend::CaptureImage>,
    pub outputs: HashSet<wl_output::WlOutput>,
}

impl Workspace {
    pub fn handle(&self) -> &ExtWorkspaceHandleV1 {
        &self.info.handle
    }

    pub fn is_active(&self) -> bool {
        self.info
            .state
            .contains(ext_workspace_handle_v1::State::Active)
    }

    pub fn can_rename(&self) -> bool {
        use cctk::cosmic_protocols::workspace::v2::client::zcosmic_workspace_handle_v2;
        self.info.cosmic_handle.is_some()
            && self
                .info
                .cosmic_capabilities
                .contains(zcosmic_workspace_handle_v2::WorkspaceCapabilities::Rename)
    }
}

#[derive(Clone)]
struct Output {
    handle: wl_output::WlOutput,
    name: String,
    width: i32,
    height: i32,
}

#[derive(Debug)]
pub struct LayerSurface {
    pub output: wl_output::WlOutput,
    last_size: Option<(Option<u32>, Option<u32>)>,
    hovered: bool,
    hidden: bool,
    // Bumped to invalidate pending hide timers
    hide_generation: u64,
    // Last pointer position, for context menu placement
    cursor: iced::Point,
}

#[derive(Debug)]
pub struct ContextMenu {
    pub id: SurfaceId,
    pub workspace: Option<(ExtWorkspaceHandleV1, String)>,
}

#[derive(Debug)]
pub struct RenameDialog {
    pub id: SurfaceId,
    pub workspace: ExtWorkspaceHandleV1,
    pub value: String,
}

/// Current MPRIS player snapshot plus any decoded album art
pub struct MediaState {
    pub info: backend::media::PlayerState,
    pub art: Option<cosmic::widget::image::Handle>,
}

// Decode a small local album art file into an RGBA image handle
fn load_art(path: &std::path::Path) -> Option<cosmic::widget::image::Handle> {
    let bytes = std::fs::read(path).ok()?;
    let img = image::load_from_memory(&bytes).ok()?.to_rgba8();
    let (w, h) = img.dimensions();
    Some(cosmic::widget::image::Handle::from_rgba(
        w,
        h,
        img.into_raw(),
    ))
}

/// A layer surface with an exclusive zone, reported by overlap-notify
#[derive(Clone, Debug)]
struct Reserved {
    output: wl_output::WlOutput,
    namespace: String,
    exclusive: u32,
    rect: iced::Rectangle,
}

pub struct App {
    core: cosmic::app::Core,
    pub config: config::Config,
    pub config_ctx: Option<cosmic::cosmic_config::Config>,
    pub panel_theme: config::PanelTheme,
    pub settings: settings::SettingsState,
    pub context_menu: Option<ContextMenu>,
    pub rename: Option<RenameDialog>,
    pub media: Option<MediaState>,
    frosted_panel: bool,
    pub layer_surfaces: HashMap<SurfaceId, LayerSurface>,
    // Invisible full-output surfaces used to receive overlap-notify events
    probe_surfaces: HashMap<SurfaceId, wl_output::WlOutput>,
    reserved: HashMap<(SurfaceId, String), Reserved>,
    outputs: Vec<Output>,
    pub workspaces: Vec<Workspace>,
    conn: Option<Connection>,
    wayland_cmd_sender: Option<calloop::channel::Sender<backend::Cmd>>,
    scroll: DiscreteScrollState,
}

impl App {
    fn workspaces_on(&self, output: &wl_output::WlOutput) -> usize {
        self.workspaces
            .iter()
            .filter(|w| w.outputs.contains(output))
            .count()
    }

    // (width, height) for a bar surface on the given output
    fn surface_size(&self, output: &wl_output::WlOutput) -> Option<(Option<u32>, Option<u32>)> {
        let cfg = &self.config;
        if cfg.fill {
            return cfg.edge.layer_size(cfg.size);
        }
        // Previews are cropped by the measured clip strips
        let aspect = self
            .outputs
            .iter()
            .find(|o| &o.handle == output)
            .and_then(|o| {
                let clips = self.edge_clips(&o.handle);
                let w = o.width as f32 - (clips[2] + clips[3]) as f32;
                let h = o.height as f32 - (clips[0] + clips[1]) as f32;
                (w > 0.0 && h > 0.0).then_some(w / h)
            })
            .unwrap_or(16.0 / 9.0);
        let len = view::bar_length(cfg.size, cfg.edge, self.workspaces_on(output), aspect);
        let len = if cfg.media_enabled && self.media.is_some() {
            len + view::media_block_length(cfg.size, cfg.edge) + view::ITEM_SPACING as u32
        } else {
            len
        };
        if cfg.edge.is_vertical() {
            Some((Some(cfg.size), Some(len)))
        } else {
            Some((Some(len), Some(cfg.size)))
        }
    }

    // Keep centered-mode surfaces sized to their content; skip no-op resizes
    fn resize_surfaces(&mut self) -> Task<cosmic::Action<Msg>> {
        if self.config.fill {
            return Task::none();
        }
        let sizes: Vec<_> = self
            .layer_surfaces
            .iter()
            .map(|(id, s)| (*id, self.surface_size(&s.output)))
            .collect();
        let mut tasks = Vec::new();
        for (id, size) in sizes {
            let surface = self.layer_surfaces.get_mut(&id).unwrap();
            if surface.last_size != size {
                surface.last_size = size;
                let (w, h) = size.unwrap_or((None, None));
                tasks.push(set_size(id, w, h));
            }
        }
        Task::batch(tasks)
    }

    fn create_surface(&mut self, output: wl_output::WlOutput) -> Task<cosmic::Action<Msg>> {
        let id = SurfaceId::unique();
        let size = self.surface_size(&output);
        self.layer_surfaces.insert(
            id,
            LayerSurface {
                output: output.clone(),
                last_size: size,
                hovered: false,
                hidden: false,
                hide_generation: 0,
                cursor: iced::Point::ORIGIN,
            },
        );
        let anchor = if self.config.fill {
            self.config.edge.anchor()
        } else {
            self.config.edge.solo_anchor()
        };
        let surface_task = get_layer_surface(SctkLayerSurfaceSettings {
            id,
            keyboard_interactivity: KeyboardInteractivity::None,
            namespace: "workspaces-bar".into(),
            layer: Layer::Top,
            size,
            output: IcedOutput::Output(output),
            anchor,
            exclusive_zone: if self.config.autohide {
                0
            } else {
                self.config.size as i32
            },
            ..Default::default()
        });
        let mut tasks = vec![surface_task, self.blur_task(id)];
        if self.config.autohide {
            tasks.push(self.schedule_hide(id));
        }
        Task::batch(tasks)
    }

    // Same condition cosmic-panel uses for its own blur, unless overridden
    pub(crate) fn blur_enabled(&self) -> bool {
        self.config
            .blur
            .unwrap_or(self.frosted_panel && self.panel_theme.opacity > 0.001)
    }

    fn blur_task(&self, id: SurfaceId) -> Task<cosmic::Action<Msg>> {
        if self.blur_enabled() {
            cosmic::iced::runtime::window::enable_blur(id)
        } else {
            cosmic::iced::runtime::window::disable_blur(id)
        }
    }

    fn apply_blur_all(&self) -> Task<cosmic::Action<Msg>> {
        Task::batch(self.layer_surfaces.keys().map(|id| self.blur_task(*id)))
    }

    fn hide_surface(&mut self, id: SurfaceId) -> Task<cosmic::Action<Msg>> {
        let Some(surface) = self.layer_surfaces.get_mut(&id) else {
            return Task::none();
        };
        surface.hidden = true;
        // Nobody sees previews while every bar is hidden; pause captures
        self.send_bar_filter();
        let (top, right, bottom, left) = hidden_margin(self.config.edge, self.config.size);
        set_margin(id, top, right, bottom, left)
    }

    fn reveal_surface(&mut self, id: SurfaceId) -> Task<cosmic::Action<Msg>> {
        if let Some(surface) = self.layer_surfaces.get_mut(&id) {
            surface.hidden = false;
            surface.hide_generation += 1;
            self.send_bar_filter();
        }
        set_margin(id, 0, 0, 0, 0)
    }

    // Hide after AUTOHIDE_WAIT unless the pointer returns first
    fn schedule_hide(&mut self, id: SurfaceId) -> Task<cosmic::Action<Msg>> {
        let Some(surface) = self.layer_surfaces.get_mut(&id) else {
            return Task::none();
        };
        surface.hide_generation += 1;
        let generation = surface.hide_generation;
        Task::future(async move {
            tokio::time::sleep(AUTOHIDE_WAIT).await;
            cosmic::Action::App(Msg::HideTimeout(id, generation))
        })
    }

    // Apply the current autohide mode to all bar surfaces
    fn apply_autohide_all(&mut self) -> Task<cosmic::Action<Msg>> {
        let autohide = self.config.autohide;
        let zone = if autohide { 0 } else { self.config.size as i32 };
        let ids: Vec<_> = self.layer_surfaces.keys().copied().collect();
        let mut tasks = Vec::new();
        for id in ids {
            tasks.push(set_exclusive_zone(id, zone));
            if autohide {
                tasks.push(self.schedule_hide(id));
            } else {
                tasks.push(self.reveal_surface(id));
            }
        }
        Task::batch(tasks)
    }

    fn destroy_surface(&mut self, output: &wl_output::WlOutput) -> Task<cosmic::Action<Msg>> {
        if let Some((id, _)) = self
            .layer_surfaces
            .iter()
            .find(|(_id, surface)| &surface.output == output)
        {
            let id = *id;
            self.layer_surfaces.remove(&id);
            destroy_layer_surface(id)
        } else {
            Task::none()
        }
    }

    fn recreate_surfaces(&mut self) -> Task<cosmic::Action<Msg>> {
        let destroy: Vec<_> = self
            .layer_surfaces
            .drain()
            .map(|(id, _)| destroy_layer_surface(id))
            .collect();
        let outputs = self.outputs.clone();
        let create: Vec<_> = outputs
            .into_iter()
            .map(|output| self.create_surface(output.handle))
            .collect();
        Task::batch(destroy.into_iter().chain(create))
    }

    // Invisible full-output surface; overlap-notify on it reports every
    // layer surface on the output, giving us all reserved (exclusive) strips
    fn create_probe(&mut self, output: wl_output::WlOutput) -> Task<cosmic::Action<Msg>> {
        let id = SurfaceId::unique();
        self.probe_surfaces.insert(id, output.clone());
        Task::batch([
            get_layer_surface(SctkLayerSurfaceSettings {
                id,
                keyboard_interactivity: KeyboardInteractivity::None,
                input_zone: Some(Vec::new()),
                namespace: "workspaces-bar-probe".into(),
                layer: Layer::Background,
                size: Some((None, None)),
                output: IcedOutput::Output(output),
                anchor: Anchor::TOP | Anchor::BOTTOM | Anchor::LEFT | Anchor::RIGHT,
                exclusive_zone: -1,
                ..Default::default()
            }),
            overlap_notify(id, true),
        ])
    }

    // Logical clip thickness per edge (top, bottom, left, right) on an output
    fn edge_clips(&self, output: &wl_output::WlOutput) -> [u32; 4] {
        let mut clips = [0u32; 4];
        if self.config.clip == config::Clip::None {
            return clips;
        }
        let Some(o) = self.outputs.iter().find(|o| &o.handle == output) else {
            return clips;
        };
        let (w, h) = (o.width as f32, o.height as f32);
        for r in self.reserved.values() {
            if r.output != *output || r.exclusive == 0 {
                continue;
            }
            if self.config.clip == config::Clip::OwnBar && r.namespace != "workspaces-bar" {
                continue;
            }
            let rect = r.rect;
            if rect.width >= rect.height {
                if rect.y + rect.height / 2.0 < h / 2.0 {
                    clips[0] = clips[0].max((rect.y + rect.height).round() as u32);
                } else {
                    clips[1] = clips[1].max((h - rect.y).round() as u32);
                }
            } else if rect.x + rect.width / 2.0 < w / 2.0 {
                clips[2] = clips[2].max((rect.x + rect.width).round() as u32);
            } else {
                clips[3] = clips[3].max((w - rect.x).round() as u32);
            }
        }
        clips
    }

    // The settings dialog is a layer surface rather than a window: toplevel
    // windows from this no-main-window app repaint in a runtime-internal loop,
    // flickering on every commit; the layer surface path renders cleanly
    fn open_settings(&mut self) -> Task<cosmic::Action<Msg>> {
        if self.settings.window.is_some() {
            return Task::none();
        }
        let id = SurfaceId::unique();
        self.settings.window = Some(id);
        self.send_bar_filter();
        get_layer_surface(SctkLayerSurfaceSettings {
            id,
            keyboard_interactivity: KeyboardInteractivity::OnDemand,
            namespace: "workspaces-bar-settings".into(),
            layer: Layer::Top,
            size: Some((Some(520), Some(720))),
            output: IcedOutput::Active,
            anchor: Anchor::empty(),
            ..Default::default()
        })
    }

    fn close_settings(&mut self) -> Task<cosmic::Action<Msg>> {
        if let Some(id) = self.settings.window.take() {
            self.send_bar_filter();
            return destroy_layer_surface(id);
        }
        Task::none()
    }

    fn open_context_menu(
        &mut self,
        parent: SurfaceId,
        workspace: Option<(ExtWorkspaceHandleV1, String)>,
    ) -> Task<cosmic::Action<Msg>> {
        let mut tasks = Vec::new();
        if let Some(menu) = self.context_menu.take() {
            tasks.push(destroy_popup(menu.id));
        }
        let Some(surface) = self.layer_surfaces.get(&parent) else {
            return Task::batch(tasks);
        };
        let cursor = surface.cursor;
        let id = SurfaceId::unique();
        // Autosize (size: None) yields a 1x1 popup; give explicit dimensions
        let size = if workspace.is_some() {
            (220, 84)
        } else {
            (220, 44)
        };
        self.context_menu = Some(ContextMenu { id, workspace });
        tasks.push(get_popup(SctkPopupSettings {
            parent,
            id,
            positioner: SctkPositioner {
                size: Some(size),
                anchor_rect: iced::Rectangle {
                    x: cursor.x as i32,
                    y: cursor.y as i32,
                    width: 1,
                    height: 1,
                },
                anchor: xdg_positioner::Anchor::BottomRight,
                gravity: xdg_positioner::Gravity::BottomRight,
                ..Default::default()
            },
            parent_size: None,
            grab: true,
            close_with_children: true,
            input_zone: None,
        }));
        Task::batch(tasks)
    }

    fn close_context_menu(&mut self) -> Task<cosmic::Action<Msg>> {
        if let Some(menu) = self.context_menu.take() {
            return destroy_popup(menu.id);
        }
        Task::none()
    }

    fn open_rename(
        &mut self,
        workspace: ExtWorkspaceHandleV1,
        value: String,
    ) -> Task<cosmic::Action<Msg>> {
        if self.rename.is_some() {
            return Task::none();
        }
        let id = SurfaceId::unique();
        self.rename = Some(RenameDialog {
            id,
            workspace,
            value,
        });
        Task::batch([
            get_layer_surface(SctkLayerSurfaceSettings {
                id,
                keyboard_interactivity: KeyboardInteractivity::Exclusive,
                namespace: "workspaces-bar-rename".into(),
                layer: Layer::Overlay,
                size: Some((Some(360), Some(150))),
                output: IcedOutput::Active,
                anchor: Anchor::empty(),
                ..Default::default()
            }),
            cosmic::widget::text_input::focus(view::RENAME_INPUT_ID.clone()),
        ])
    }

    fn close_rename(&mut self) -> Task<cosmic::Action<Msg>> {
        if let Some(dialog) = self.rename.take() {
            return destroy_layer_surface(dialog.id);
        }
        Task::none()
    }

    fn send_wayland_cmd(&self, cmd: backend::Cmd) {
        if let Some(sender) = self.wayland_cmd_sender.as_ref() {
            let _ = sender.send(cmd);
        }
    }

    // Tell the backend where the bar is, so self-caused damage can be ignored
    fn send_bar_filter(&self) {
        let all_hidden = self.config.autohide
            && !self.layer_surfaces.is_empty()
            && self.layer_surfaces.values().all(|s| s.hidden);
        self.send_wayland_cmd(backend::Cmd::BarFilter {
            edge: self.config.edge,
            size: self.config.size,
            outputs: self
                .outputs
                .iter()
                .map(|o| (o.handle.clone(), (o.width, o.height)))
                .collect(),
            paused: self.settings.window.is_some() || all_hidden,
            // Preview resolution scaled to the bar size; ~2x covers aspect + hidpi
            preview_px: (self.config.size * 2).clamp(128, 512),
            clips: self
                .outputs
                .iter()
                .map(|o| (o.handle.clone(), self.edge_clips(&o.handle)))
                .collect(),
        });
    }
}

impl Application for App {
    type Message = Msg;
    type Executor = cosmic::SingleThreadExecutor;
    type Flags = Args;
    const APP_ID: &'static str = config::APP_ID;

    fn init(core: cosmic::app::Core, flags: Args) -> (Self, Task<cosmic::Action<Msg>>) {
        let config = config::load();
        let mut app = App {
            core,
            config_ctx: config::context(),
            settings: settings::SettingsState::new(&config),
            config,
            panel_theme: config::PanelTheme::default(),
            frosted_panel: cosmic::theme::active().cosmic().frosted_panel,
            context_menu: None,
            rename: None,
            media: None,
            layer_surfaces: HashMap::new(),
            probe_surfaces: HashMap::new(),
            reserved: HashMap::new(),
            outputs: Vec::new(),
            workspaces: Vec::new(),
            conn: None,
            wayland_cmd_sender: None,
            scroll: DiscreteScrollState::default(),
        };
        let task = if flags.action.is_some() {
            app.open_settings()
        } else {
            Task::none()
        };
        (app, task)
    }

    fn update(&mut self, message: Msg) -> Task<cosmic::Action<Msg>> {
        match message {
            Msg::WaylandEvent(evt) => match evt {
                WaylandEvent::Output(evt, output) => {
                    if self.conn.is_none()
                        && let Some(wl_backend) = output.backend().upgrade()
                    {
                        self.conn = Some(Connection::from_backend(wl_backend));
                    }

                    match evt {
                        OutputEvent::Created(Some(info)) => {
                            if let (Some(name), Some((width, height))) =
                                (info.name, info.logical_size)
                            {
                                self.outputs.push(Output {
                                    handle: output.clone(),
                                    name,
                                    width,
                                    height,
                                });
                                self.send_bar_filter();
                                return Task::batch([
                                    self.create_surface(output.clone()),
                                    self.create_probe(output),
                                ]);
                            }
                        }
                        OutputEvent::Created(None) => {}
                        OutputEvent::InfoUpdate(info) => {
                            if let Some(o) = self.outputs.iter_mut().find(|x| x.handle == output) {
                                if let Some(name) = info.name {
                                    o.name = name;
                                }
                                if let Some((width, height)) = info.logical_size {
                                    o.width = width;
                                    o.height = height;
                                }
                            }
                            self.send_bar_filter();
                            return self.resize_surfaces();
                        }
                        OutputEvent::Removed => {
                            if let Some(idx) = self.outputs.iter().position(|x| x.handle == output)
                            {
                                self.outputs.remove(idx);
                            }
                            self.reserved.retain(|_, r| r.output != output);
                            let probe = self
                                .probe_surfaces
                                .iter()
                                .find(|(_, o)| **o == output)
                                .map(|(id, _)| *id);
                            self.send_bar_filter();
                            let mut tasks = vec![self.destroy_surface(&output)];
                            if let Some(id) = probe {
                                self.probe_surfaces.remove(&id);
                                tasks.push(destroy_layer_surface(id));
                            }
                            return Task::batch(tasks);
                        }
                    }
                }
                WaylandEvent::Layer(LayerEvent::Done, _surface, id) => {
                    if self.settings.window == Some(id) {
                        self.settings.window = None;
                        self.send_bar_filter();
                    }
                    if self.rename.as_ref().is_some_and(|d| d.id == id) {
                        self.rename = None;
                    }
                    if self.probe_surfaces.remove(&id).is_some() {
                        self.reserved.retain(|(probe, _), _| *probe != id);
                    }
                    self.layer_surfaces.remove(&id);
                }
                WaylandEvent::Popup(PopupEvent::Done, _surface, id) => {
                    if self.context_menu.as_ref().is_some_and(|m| m.id == id) {
                        self.context_menu = None;
                    }
                }
                WaylandEvent::OverlapNotify(event, _surface, id) => {
                    if let Some(output) = self.probe_surfaces.get(&id).cloned() {
                        let changed = match event {
                            OverlapNotifyEvent::OverlapLayerAdd {
                                identifier,
                                namespace,
                                exclusive,
                                logical_rect,
                                ..
                            } => {
                                self.reserved.insert(
                                    (id, identifier),
                                    Reserved {
                                        output,
                                        namespace,
                                        exclusive,
                                        rect: logical_rect,
                                    },
                                );
                                true
                            }
                            OverlapNotifyEvent::OverlapLayerRemove { identifier } => {
                                self.reserved.remove(&(id, identifier)).is_some()
                            }
                            _ => false,
                        };
                        if changed {
                            self.send_bar_filter();
                            return self.resize_surfaces();
                        }
                    }
                }
                _ => {}
            },
            Msg::Wayland(evt) => match evt {
                backend::Event::CmdSender(sender) => {
                    self.wayland_cmd_sender = Some(sender);
                    self.send_bar_filter();
                }
                backend::Event::Workspaces(mut workspaces) => {
                    workspaces.sort_by(|(_, w1), (_, w2)| w1.coordinates.cmp(&w2.coordinates));
                    let old_workspaces = mem::take(&mut self.workspaces);
                    for (outputs, workspace) in workspaces {
                        let img = old_workspaces
                            .iter()
                            .find(|w| w.handle() == &workspace.handle)
                            .and_then(|w| w.img.clone());
                        self.workspaces.push(Workspace {
                            info: workspace,
                            img,
                            outputs,
                        });
                    }
                    return self.resize_surfaces();
                }
                backend::Event::WorkspaceCapture(handle, image) => {
                    if let Some(workspace) =
                        self.workspaces.iter_mut().find(|w| w.handle() == &handle)
                    {
                        workspace.img = Some(image);
                    }
                }
            },
            Msg::Config(new_config) => {
                if new_config != self.config {
                    let geometry_changed = new_config.size != self.config.size
                        || new_config.edge != self.config.edge
                        || new_config.fill != self.config.fill;
                    let blur_changed = new_config.blur != self.config.blur;
                    let autohide_changed = new_config.autohide != self.config.autohide;
                    let clip_changed = new_config.clip != self.config.clip;
                    self.config = new_config;
                    settings::sync_pickers(self);
                    if geometry_changed {
                        self.send_bar_filter();
                        return self.recreate_surfaces();
                    }
                    let mut tasks = Vec::new();
                    if blur_changed {
                        tasks.push(self.apply_blur_all());
                    }
                    if autohide_changed {
                        tasks.push(self.apply_autohide_all());
                    }
                    if autohide_changed || clip_changed {
                        self.send_bar_filter();
                        // Clip changes alter preview aspect in centered mode
                        tasks.push(self.resize_surfaces());
                    }
                    if !tasks.is_empty() {
                        return Task::batch(tasks);
                    }
                }
            }
            Msg::PanelTheme(panel_theme) => {
                if panel_theme != self.panel_theme {
                    self.panel_theme = panel_theme;
                    return self.apply_blur_all();
                }
            }
            Msg::ActivateWorkspace(workspace_handle) => {
                self.send_wayland_cmd(backend::Cmd::ActivateWorkspace(workspace_handle));
            }
            Msg::OnScroll(output, delta) => {
                let discrete_delta = self.scroll.update(delta);
                if discrete_delta.y != 0 {
                    let workspaces: Vec<_> = self
                        .workspaces
                        .iter()
                        .filter(|w| w.outputs.contains(&output))
                        .collect();
                    if let Some(workspace_idx) = workspaces.iter().position(|i| i.is_active()) {
                        let new_workspace_idx = (workspace_idx as isize - discrete_delta.y)
                            .rem_euclid(workspaces.len() as isize)
                            as usize;
                        let workspace = workspaces[new_workspace_idx];
                        self.send_wayland_cmd(backend::Cmd::ActivateWorkspace(
                            workspace.handle().clone(),
                        ));
                    }
                }
            }
            Msg::BarEnter(id) => {
                if self.config.autohide
                    && let Some(surface) = self.layer_surfaces.get_mut(&id)
                {
                    surface.hovered = true;
                    surface.hide_generation += 1;
                    if surface.hidden {
                        return self.reveal_surface(id);
                    }
                }
            }
            Msg::BarExit(id) => {
                if self.config.autohide
                    && let Some(surface) = self.layer_surfaces.get_mut(&id)
                {
                    surface.hovered = false;
                    return self.schedule_hide(id);
                }
            }
            Msg::HideTimeout(id, generation) => {
                if self.config.autohide
                    && let Some(surface) = self.layer_surfaces.get(&id)
                    && surface.hide_generation == generation
                    && !surface.hovered
                    && !surface.hidden
                {
                    return self.hide_surface(id);
                }
            }
            Msg::BarMove(id, point) => {
                if let Some(surface) = self.layer_surfaces.get_mut(&id) {
                    surface.cursor = point;
                }
            }
            Msg::OpenContextMenu(id, workspace) => {
                // Right-press isn't captured by nested mouse_areas, so the
                // bar-level handler also fires; don't clobber the item's menu
                if workspace.is_none() && self.context_menu.is_some() {
                    return Task::none();
                }
                return self.open_context_menu(id, workspace);
            }
            Msg::MenuRename => {
                let workspace = self
                    .context_menu
                    .as_ref()
                    .and_then(|m| m.workspace.clone());
                let close = self.close_context_menu();
                if let Some((handle, name)) = workspace {
                    return Task::batch([close, self.open_rename(handle, name)]);
                }
                return close;
            }
            Msg::MenuSettings => {
                let close = self.close_context_menu();
                return Task::batch([close, self.open_settings()]);
            }
            Msg::RenameInput(value) => {
                if let Some(dialog) = self.rename.as_mut() {
                    dialog.value = value;
                }
            }
            Msg::RenameSubmit => {
                if let Some(dialog) = self.rename.as_ref() {
                    self.send_wayland_cmd(backend::Cmd::RenameWorkspace(
                        dialog.workspace.clone(),
                        dialog.value.clone(),
                    ));
                }
                return self.close_rename();
            }
            Msg::RenameCancel => {
                return self.close_rename();
            }
            Msg::OpenSettings => {
                return self.open_settings();
            }
            Msg::Settings(msg) => {
                return settings::update(self, msg);
            }
            Msg::CloseSettings => {
                return self.close_settings();
            }
            Msg::Media(backend::media::Event::Player(state)) => {
                self.media = state.map(|info| {
                    let art = info.art_path.as_deref().and_then(load_art);
                    MediaState { info, art }
                });
                if self.config.media_enabled {
                    return self.resize_surfaces();
                }
            }
            Msg::MediaControl(control) => {
                if let Some(media) = self.media.as_ref() {
                    let bus_name = media.info.bus_name.clone();
                    return Task::future(async move {
                        backend::media::send_control(bus_name, control).await;
                        cosmic::Action::App(Msg::Ignore)
                    });
                }
            }
            Msg::CloseWindow(id) => {
                if self.settings.window == Some(id) {
                    return self.close_settings();
                }
            }
            Msg::Ignore => {}
        }
        Task::none()
    }

    fn dbus_activation(
        &mut self,
        msg: cosmic::dbus_activation::Message,
    ) -> Task<cosmic::Action<Msg>> {
        use cosmic::dbus_activation::Details;
        match msg.msg {
            Details::Activate | Details::ActivateAction { .. } => self.open_settings(),
            _ => Task::none(),
        }
    }

    fn system_theme_update(
        &mut self,
        _keys: &[&'static str],
        new_theme: &cosmic::cosmic_theme::Theme,
    ) -> Task<cosmic::Action<Msg>> {
        self.frosted_panel = new_theme.frosted_panel;
        self.apply_blur_all()
    }

    fn subscription(&self) -> Subscription<Msg> {
        // Forward only the events we handle; forwarding Frame events would
        // loop: commit -> frame callback -> message -> redraw -> commit
        let events = iced::event::listen_with(|evt, _, _| match evt {
            iced::Event::PlatformSpecific(iced::event::PlatformSpecific::Wayland(evt)) => {
                match &evt {
                    WaylandEvent::Output(..)
                    | WaylandEvent::Layer(..)
                    | WaylandEvent::Popup(..)
                    // Toplevel overlap events fire for every window; unused
                    | WaylandEvent::OverlapNotify(
                        OverlapNotifyEvent::OverlapLayerAdd { .. }
                        | OverlapNotifyEvent::OverlapLayerRemove { .. },
                        ..,
                    ) => Some(Msg::WaylandEvent(evt)),
                    _ => None,
                }
            }
            _ => None,
        });

        let config_subscription = cosmic_config::config_subscription::<_, config::Config>(
            "config-sub",
            config::APP_ID.into(),
            config::CONFIG_VERSION,
        )
        .map(|update| {
            if !update.errors.is_empty() {
                log::debug!("config errors: {:?}", update.errors);
            }
            Msg::Config(update.config)
        });

        let panel_theme_subscription =
            cosmic_config::config_subscription::<_, config::PanelTheme>(
                "panel-theme-sub",
                config::PANEL_CONFIG_ID.into(),
                1,
            )
            .map(|update| {
                if !update.errors.is_empty() {
                    log::debug!("panel config errors: {:?}", update.errors);
                }
                Msg::PanelTheme(update.config)
            });

        let mut subscriptions = vec![events, config_subscription, panel_theme_subscription];
        if self.config.media_enabled {
            subscriptions.push(backend::media::subscription().map(Msg::Media));
        }
        if let Some(conn) = self.conn.clone() {
            subscriptions.push(backend::subscription(conn).map(Msg::Wayland));
        }
        Subscription::batch(subscriptions)
    }

    fn view(&self) -> cosmic::Element<'_, Msg> {
        // No main window; everything is drawn in layer surfaces
        cosmic::widget::text("").into()
    }

    fn view_window(&self, id: SurfaceId) -> cosmic::Element<'_, Msg> {
        if self.settings.window == Some(id) {
            return settings::view(self);
        }
        if self.context_menu.as_ref().is_some_and(|m| m.id == id) {
            return view::menu_view(self);
        }
        if self.rename.as_ref().is_some_and(|d| d.id == id) {
            return view::rename_view(self);
        }
        if let Some(surface) = self.layer_surfaces.get(&id) {
            return view::bar_view(self, id, surface);
        }
        cosmic::widget::text("").into()
    }

    fn on_close_requested(&self, id: SurfaceId) -> Option<Msg> {
        Some(Msg::CloseWindow(id))
    }

    fn core(&self) -> &cosmic::app::Core {
        &self.core
    }

    fn core_mut(&mut self) -> &mut cosmic::app::Core {
        &mut self.core
    }
}

#[derive(Clone, Debug, Default)]
pub struct Args {
    action: Option<String>,
}

impl CosmicFlags for Args {
    type SubCommand = String;
    type Args = Vec<String>;

    fn action(&self) -> Option<&String> {
        self.action.as_ref()
    }
}

fn main() -> iced::Result {
    env_logger::init();

    let settings_flag = std::env::args().any(|a| a == "--settings");
    cosmic::app::run_single_instance::<App>(
        cosmic::app::Settings::default()
            .no_main_window(true)
            .exit_on_close(false),
        Args {
            action: settings_flag.then(|| "settings".to_string()),
        },
    )
}
