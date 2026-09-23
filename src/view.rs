// SPDX-License-Identifier: GPL-3.0-only
// Styling based on cosmic-workspaces-epoch, Copyright 2023 System76 <info@system76.com>

use cosmic::cctk::wayland_protocols::ext::workspace::v1::client::ext_workspace_handle_v1;
use cosmic::iced::{self, Alignment, Length};
use cosmic::widget;
use std::sync::LazyLock;

use crate::config::{Config, Edge, PanelBackground, PanelTheme};
use crate::{App, LayerSurface, Msg, Workspace};

pub static RENAME_INPUT_ID: LazyLock<widget::Id> =
    LazyLock::new(|| widget::Id::new("rename-input"));

pub const BAR_PADDING: f32 = 8.0;
pub const ITEM_SPACING: f32 = 8.0;
pub const ITEM_PADDING: f32 = 4.0;
pub const ITEM_INNER_SPACING: f32 = 4.0;
pub const CAPTION_HEIGHT: f32 = 20.0;

/// Theme override values copied out of `Config` for use in style closures
#[derive(Clone, Copy)]
pub struct Overrides {
    pub bg_color: Option<[f32; 4]>,
    pub bar_radius: Option<u32>,
    pub active_border_color: Option<[f32; 3]>,
    pub active_border_width: Option<f32>,
    pub hover_color: Option<[f32; 4]>,
    pub item_radius: Option<u32>,
    pub text_color: Option<[f32; 3]>,
}

impl From<&Config> for Overrides {
    fn from(c: &Config) -> Self {
        Self {
            bg_color: c.bg_color,
            bar_radius: c.bar_radius,
            active_border_color: c.active_border_color,
            active_border_width: c.active_border_width,
            hover_color: c.hover_color,
            item_radius: c.item_radius,
            text_color: c.text_color,
        }
    }
}

fn rgba(c: [f32; 4]) -> iced::Color {
    iced::Color::from_rgba(c[0], c[1], c[2], c[3])
}

fn rgb(c: [f32; 3]) -> iced::Color {
    iced::Color::from_rgb(c[0], c[1], c[2])
}

/// Preview thickness available across the bar, accounting for padding and caption
pub fn preview_thickness(size: u32, edge: Edge) -> f32 {
    let mut t = size as f32 - 2.0 * (BAR_PADDING + ITEM_PADDING);
    if !edge.is_vertical() {
        t -= CAPTION_HEIGHT + ITEM_INNER_SPACING;
    }
    t.max(32.0)
}

/// Bar length along the edge for shrink-to-content mode
pub fn bar_length(size: u32, edge: Edge, workspaces: usize, output_aspect: f32) -> u32 {
    let n = workspaces.max(1) as f32;
    let thickness = preview_thickness(size, edge);
    let item_len = if edge.is_vertical() {
        thickness / output_aspect + ITEM_INNER_SPACING + CAPTION_HEIGHT + 2.0 * ITEM_PADDING
    } else {
        thickness * output_aspect + 2.0 * ITEM_PADDING
    };
    (n * item_len + (n - 1.0) * ITEM_SPACING + 2.0 * BAR_PADDING).ceil() as u32
}

pub fn bar_view<'a>(
    app: &'a App,
    id: iced::window::Id,
    surface: &'a LayerSurface,
) -> cosmic::Element<'a, Msg> {
    let edge = app.config.edge;
    let ov = Overrides::from(&app.config);
    let thickness = preview_thickness(app.config.size, edge);

    let items = app
        .workspaces
        .iter()
        .filter(|w| w.outputs.contains(&surface.output))
        .map(|w| workspace_item(w, id, edge, thickness, ov));

    let content: cosmic::Element<'_, Msg> = if edge.is_vertical() {
        widget::column::with_children(items)
            .spacing(ITEM_SPACING)
            .padding(BAR_PADDING)
            .align_x(Alignment::Center)
            .width(Length::Fill)
            .into()
    } else {
        widget::row::with_children(items)
            .spacing(ITEM_SPACING)
            .padding(BAR_PADDING)
            .align_y(Alignment::Center)
            .height(Length::Fill)
            .into()
    };

    let panel = app.panel_theme.clone();
    let round_edge = app.config.round_edge_corners;
    let bar = widget::container(content)
        .class(cosmic::theme::Container::custom(move |theme| {
            let bg = ov
                .bg_color
                .map(rgba)
                .unwrap_or_else(|| panel_bg_color(theme, &panel));
            let r = ov.bar_radius.unwrap_or(panel.border_radius) as f32;
            let mut radius = iced::border::Radius::from(r);
            if !round_edge {
                match edge {
                    Edge::Top => {
                        radius.top_left = 0.0;
                        radius.top_right = 0.0;
                    }
                    Edge::Bottom => {
                        radius.bottom_left = 0.0;
                        radius.bottom_right = 0.0;
                    }
                    Edge::Left => {
                        radius.top_left = 0.0;
                        radius.bottom_left = 0.0;
                    }
                    Edge::Right => {
                        radius.top_right = 0.0;
                        radius.bottom_right = 0.0;
                    }
                }
            }
            cosmic::iced::widget::container::Style {
                background: Some(iced::Background::Color(bg)),
                border: iced::Border {
                    radius,
                    ..Default::default()
                },
                ..Default::default()
            }
        }))
        .width(Length::Fill)
        .height(Length::Fill);

    let output = surface.output.clone();
    let mut area = widget::mouse_area(bar)
        .on_scroll(move |delta| Msg::OnScroll(output.clone(), delta))
        .on_move(move |p| Msg::BarMove(id, p))
        .on_right_press(Msg::OpenContextMenu(id, None));
    if app.config.autohide {
        area = area.on_enter(Msg::BarEnter(id)).on_exit(Msg::BarExit(id));
    }
    area.into()
}

/// Background color following the COSMIC panel's appearance settings
pub fn panel_bg_color(theme: &cosmic::Theme, panel: &PanelTheme) -> iced::Color {
    let cosmic = theme.cosmic();
    let mut color: iced::Color = match panel.background {
        PanelBackground::ThemeDefault => cosmic.bg_color().into(),
        PanelBackground::Dark => cosmic::cosmic_theme::Theme::dark_default().bg_color().into(),
        PanelBackground::Light => cosmic::cosmic_theme::Theme::light_default().bg_color().into(),
        PanelBackground::Color([r, g, b]) => iced::Color::from_rgb(r, g, b),
    };
    let mut alpha = panel.opacity;
    // Frosted glass: panel lowers alpha and relies on compositor blur
    if cosmic.frosted_panel {
        alpha *= cosmic.alpha_map.blurred_alpha(cosmic.frosted);
    }
    color.a = alpha;
    color
}

fn workspace_item_appearance(
    theme: &cosmic::Theme,
    is_active: bool,
    hovered: bool,
    ov: &Overrides,
) -> cosmic::widget::button::Style {
    let cosmic = theme.cosmic();
    let mut appearance = cosmic::widget::button::Style::new();
    appearance.border_radius = match ov.item_radius {
        Some(r) => (r as f32).into(),
        None => cosmic
            .corner_radii
            .radius_s
            .map(|x| if x < 4.0 { x } else { x + 4.0 })
            .into(),
    };
    if is_active {
        appearance.border_width = ov.active_border_width.unwrap_or(4.0);
        appearance.border_color = ov
            .active_border_color
            .map(rgb)
            .unwrap_or_else(|| cosmic.accent.base.into());
    }
    if hovered {
        let bg = ov
            .hover_color
            .map(rgba)
            .unwrap_or_else(|| cosmic.button.base.into());
        appearance.background = Some(iced::Background::Color(bg));
    }
    appearance
}

fn label_class(ov: &Overrides) -> cosmic::theme::Text {
    match ov.text_color {
        Some(c) => cosmic::theme::Text::Color(rgb(c)),
        None => cosmic::theme::Text::Default,
    }
}

fn workspace_item(
    workspace: &Workspace,
    surface_id: iced::window::Id,
    edge: Edge,
    thickness: f32,
    ov: Overrides,
) -> cosmic::Element<'_, Msg> {
    let preview: cosmic::Element<'_, Msg> = match &workspace.img {
        Some(img) => {
            let aspect = img.width.max(1) as f32 / img.height.max(1) as f32;
            let (width, height) = if edge.is_vertical() {
                (thickness, thickness / aspect)
            } else {
                (thickness * aspect, thickness)
            };
            widget::Image::new(img.image.clone())
                .content_fit(iced::ContentFit::Contain)
                .width(Length::Fixed(width))
                .height(Length::Fixed(height))
                .into()
        }
        None => {
            let (width, height) = if edge.is_vertical() {
                (thickness, thickness * 9.0 / 16.0)
            } else {
                (thickness * 16.0 / 9.0, thickness)
            };
            widget::container(widget::text::body(&workspace.info.name).class(label_class(&ov)))
                .center(Length::Fill)
                .width(Length::Fixed(width))
                .height(Length::Fixed(height))
                .into()
        }
    };

    let label = widget::text::caption(&workspace.info.name).class(label_class(&ov));

    let content = widget::column::with_children(vec![preview, label.into()])
        .spacing(ITEM_INNER_SPACING)
        .align_x(Alignment::Center);

    let is_active = workspace.is_active();
    let mut button = widget::button::custom(content)
        .selected(is_active)
        .class(cosmic::theme::Button::Custom {
            active: Box::new(move |_focused, theme| {
                workspace_item_appearance(theme, is_active, false, &ov)
            }),
            disabled: Box::new(move |theme| {
                workspace_item_appearance(theme, is_active, false, &ov)
            }),
            hovered: Box::new(move |_focused, theme| {
                workspace_item_appearance(theme, is_active, true, &ov)
            }),
            pressed: Box::new(move |_focused, theme| {
                workspace_item_appearance(theme, is_active, true, &ov)
            }),
        })
        .padding(ITEM_PADDING as u16);
    if workspace
        .info
        .capabilities
        .contains(ext_workspace_handle_v1::WorkspaceCapabilities::Activate)
    {
        button = button.on_press(Msg::ActivateWorkspace(workspace.handle().clone()));
    }

    if workspace.can_rename() {
        let ctx = Some((workspace.handle().clone(), workspace.info.name.clone()));
        widget::mouse_area(button)
            .on_right_press(Msg::OpenContextMenu(surface_id, ctx))
            .into()
    } else {
        button.into()
    }
}

fn menu_item(label: &str, msg: Msg) -> cosmic::Element<'_, Msg> {
    widget::button::custom(widget::text::body(label))
        .class(cosmic::theme::Button::MenuItem)
        .width(Length::Fill)
        .padding([4, 12])
        .on_press(msg)
        .into()
}

pub fn menu_view(app: &App) -> cosmic::Element<'_, Msg> {
    let has_workspace = app
        .context_menu
        .as_ref()
        .is_some_and(|m| m.workspace.is_some());
    let mut col = widget::column::with_capacity(3)
        .width(Length::Fill)
        .padding(4.0);
    if has_workspace {
        col = col.push(menu_item("Rename workspace\u{2026}", Msg::MenuRename));
        col = col.push(
            widget::container(widget::divider::horizontal::light()).padding([4, 0]),
        );
    }
    col = col.push(menu_item("Settings", Msg::MenuSettings));
    widget::container(col)
        .class(cosmic::theme::Container::Dropdown)
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
}

pub fn rename_view(app: &App) -> cosmic::Element<'_, Msg> {
    let value = app.rename.as_ref().map_or("", |d| d.value.as_str());
    let content = widget::column::with_capacity(3)
        .spacing(12.0)
        .padding(16.0)
        .push(widget::text::title4("Rename workspace"))
        .push(
            widget::text_input("Workspace name", value)
                .id(RENAME_INPUT_ID.clone())
                .on_input(Msg::RenameInput)
                .on_submit(|_| Msg::RenameSubmit),
        )
        .push(
            widget::row::with_capacity(2)
                .spacing(8.0)
                .push(widget::button::standard("Cancel").on_press(Msg::RenameCancel))
                .push(widget::button::suggested("Rename").on_press(Msg::RenameSubmit)),
        )
        .align_x(Alignment::End);
    widget::container(content)
        .class(cosmic::theme::Container::WindowBackground)
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
}
