// SPDX-License-Identifier: GPL-3.0-only

use cosmic::cctk::sctk::shell::wlr_layer::Anchor;
use cosmic_config::{CosmicConfigEntry, cosmic_config_derive::CosmicConfigEntry};
use serde::{Deserialize, Serialize};

pub const APP_ID: &str = "com.github.antony.CosmicWorkspacesBar";
pub const CONFIG_VERSION: u64 = 1;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Edge {
    Top,
    Bottom,
    Left,
    #[default]
    Right,
}

impl Edge {
    pub fn is_vertical(self) -> bool {
        matches!(self, Edge::Left | Edge::Right)
    }

    pub fn anchor(self) -> Anchor {
        match self {
            Edge::Top => Anchor::TOP | Anchor::LEFT | Anchor::RIGHT,
            Edge::Bottom => Anchor::BOTTOM | Anchor::LEFT | Anchor::RIGHT,
            Edge::Left => Anchor::LEFT | Anchor::TOP | Anchor::BOTTOM,
            Edge::Right => Anchor::RIGHT | Anchor::TOP | Anchor::BOTTOM,
        }
    }

    /// (width, height) for the layer surface; `None` stretches along the edge
    pub fn layer_size(self, size: u32) -> Option<(Option<u32>, Option<u32>)> {
        if self.is_vertical() {
            Some((Some(size), None))
        } else {
            Some((None, Some(size)))
        }
    }

    /// Anchor for shrink-to-content mode; compositor centers a solo-anchored surface
    pub fn solo_anchor(self) -> Anchor {
        match self {
            Edge::Top => Anchor::TOP,
            Edge::Bottom => Anchor::BOTTOM,
            Edge::Left => Anchor::LEFT,
            Edge::Right => Anchor::RIGHT,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Clip {
    /// Crop this bar's reserved strip out of workspace previews
    #[default]
    OwnBar,
    /// Crop every exclusive bar/panel/dock strip out of previews
    AllBars,
    /// Show previews uncropped
    None,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum MediaPosition {
    #[default]
    Start,
    End,
}

#[derive(Clone, Debug, PartialEq, CosmicConfigEntry)]
pub struct Config {
    /// Thickness of the bar in logical pixels
    pub size: u32,
    /// Screen edge the bar is attached to
    pub edge: Edge,
    /// true: stretch along the full edge; false: shrink to content, centered
    pub fill: bool,
    /// Slide off-screen when not hovered, like the COSMIC dock
    pub autohide: bool,
    /// Round the bar corners that touch the screen edge
    pub round_edge_corners: bool,
    /// Which reserved bar strips to crop out of workspace previews
    pub clip: Clip,
    /// Show MPRIS media controls (previous/play-pause/next + title/art)
    pub media_enabled: bool,
    /// Where the media controls appear along the bar
    pub media_position: MediaPosition,
    // Theme overrides; `None` = follow the top bar / system theme
    pub bg_color: Option<[f32; 4]>,
    pub blur: Option<bool>,
    pub bar_radius: Option<u32>,
    pub active_border_color: Option<[f32; 3]>,
    pub active_border_width: Option<f32>,
    pub hover_color: Option<[f32; 4]>,
    pub item_radius: Option<u32>,
    pub text_color: Option<[f32; 3]>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            size: 256,
            edge: Edge::Right,
            fill: true,
            autohide: false,
            round_edge_corners: true,
            clip: Clip::default(),
            media_enabled: false,
            media_position: MediaPosition::default(),
            bg_color: None,
            blur: None,
            bar_radius: None,
            active_border_color: None,
            active_border_width: None,
            hover_color: None,
            item_radius: None,
            text_color: None,
        }
    }
}

pub fn context() -> Option<cosmic_config::Config> {
    match cosmic_config::Config::new(APP_ID, CONFIG_VERSION) {
        Ok(ctx) => Some(ctx),
        Err(err) => {
            log::warn!("failed to open config context: {err}");
            None
        }
    }
}

pub fn load() -> Config {
    match cosmic_config::Config::new(APP_ID, CONFIG_VERSION) {
        Ok(ctx) => match Config::get_entry(&ctx) {
            Ok(config) => config,
            Err((errors, config)) => {
                for err in errors {
                    if err.is_err() {
                        log::warn!("config load error: {err}");
                    }
                }
                config
            }
        },
        Err(err) => {
            log::warn!("failed to open config: {err}");
            Config::default()
        }
    }
}

pub const PANEL_CONFIG_ID: &str = "com.system76.CosmicPanel.Panel";

/// Mirrors the RON serialization of `CosmicPanelBackground`
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub enum PanelBackground {
    #[default]
    ThemeDefault,
    Dark,
    Light,
    Color([f32; 3]),
}

/// Subset of the COSMIC panel's config used to match its appearance
#[derive(Clone, Debug, PartialEq, CosmicConfigEntry)]
pub struct PanelTheme {
    pub background: PanelBackground,
    pub opacity: f32,
    pub border_radius: u32,
}

impl Default for PanelTheme {
    fn default() -> Self {
        Self {
            background: PanelBackground::ThemeDefault,
            opacity: 1.0,
            border_radius: 0,
        }
    }
}
