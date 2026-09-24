// SPDX-License-Identifier: GPL-3.0-only

//! Settings window: layout options and theme overrides.

use cosmic::cosmic_config::ConfigSet;
use cosmic::iced::{self, Length};
use cosmic::widget::{self, color_picker::ColorPickerUpdate, settings, ColorPickerModel};
use cosmic::{iced::Task, Element};
use serde::Serialize;

use crate::config::{Clip, Config, Edge, MediaPosition};
use crate::{App, Msg};

const EDGE_LABELS: &[&str] = &["Top", "Bottom", "Left", "Right"];
const EDGES: &[Edge] = &[Edge::Top, Edge::Bottom, Edge::Left, Edge::Right];
const CLIP_LABELS: &[&str] = &["This bar", "All bars", "Nothing"];
const CLIPS: &[Clip] = &[Clip::OwnBar, Clip::AllBars, Clip::None];
const MEDIA_POSITION_LABELS: &[&str] = &["Start", "End"];
const MEDIA_POSITIONS: &[MediaPosition] = &[MediaPosition::Start, MediaPosition::End];

pub struct SettingsState {
    pub window: Option<iced::window::Id>,
    pub bg_picker: ColorPickerModel,
    pub active_border_picker: ColorPickerModel,
    pub hover_picker: ColorPickerModel,
    pub text_picker: ColorPickerModel,
}

fn picker(initial: Option<iced::Color>) -> ColorPickerModel {
    // Fallback = saved color, so the drawer's Reset reverts staged edits
    ColorPickerModel::new("Hex", "RGB", initial, initial)
}

impl SettingsState {
    pub fn new(config: &Config) -> Self {
        Self {
            window: None,
            bg_picker: picker(config.bg_color.map(rgba)),
            active_border_picker: picker(config.active_border_color.map(rgb)),
            hover_picker: picker(config.hover_color.map(rgba)),
            text_picker: picker(config.text_color.map(rgb)),
        }
    }
}

fn rgba(c: [f32; 4]) -> iced::Color {
    iced::Color::from_rgba(c[0], c[1], c[2], c[3])
}

fn rgb(c: [f32; 3]) -> iced::Color {
    iced::Color::from_rgb(c[0], c[1], c[2])
}

fn to_rgba(c: iced::Color) -> [f32; 4] {
    [c.r, c.g, c.b, c.a]
}

fn to_rgb(c: iced::Color) -> [f32; 3] {
    [c.r, c.g, c.b]
}

#[derive(Clone, Debug)]
pub enum SettingsMsg {
    Edge(usize),
    Size(u32),
    Fill(bool),
    Autohide(bool),
    Clip(usize),
    RoundEdgeCorners(bool),
    MediaEnabled(bool),
    MediaPosition(usize),
    PreferredPlayer(String),
    MediaVolumeScroll(bool),
    OverrideBg(bool),
    BgPicker(ColorPickerUpdate),
    OverrideBlur(bool),
    Blur(bool),
    OverrideBarRadius(bool),
    BarRadius(u32),
    OverrideActiveBorderColor(bool),
    ActiveBorderPicker(ColorPickerUpdate),
    OverrideActiveBorderWidth(bool),
    ActiveBorderWidth(u32),
    OverrideHover(bool),
    HoverPicker(ColorPickerUpdate),
    OverrideItemRadius(bool),
    ItemRadius(u32),
    OverrideText(bool),
    TextPicker(ColorPickerUpdate),
    ResetAll,
}

fn write<T: Serialize>(app: &App, key: &str, value: T) {
    if let Some(ctx) = app.config_ctx.as_ref()
        && let Err(err) = ctx.set(key, value)
    {
        log::warn!("failed to write config key {key}: {err}");
    }
}

// Persist on applied/drag-finished/reset; the picker commits its staged color
// internally on ActionFinished, so config must follow or sync_pickers reverts it
fn is_commit(update: &ColorPickerUpdate) -> bool {
    matches!(
        update,
        ColorPickerUpdate::AppliedColor
            | ColorPickerUpdate::ActionFinished
            | ColorPickerUpdate::Reset
    )
}

fn sync_picker(p: &mut ColorPickerModel, color: Option<iced::Color>) {
    if !p.get_is_active() && p.get_applied_color() != color {
        *p = picker(color);
    }
}

/// Rebuild pickers whose color no longer matches config (reset, external edits)
pub fn sync_pickers(app: &mut App) {
    let bg = app.config.bg_color.map(rgba);
    let border = app.config.active_border_color.map(rgb);
    let hover = app.config.hover_color.map(rgba);
    let text = app.config.text_color.map(rgb);
    sync_picker(&mut app.settings.bg_picker, bg);
    sync_picker(&mut app.settings.active_border_picker, border);
    sync_picker(&mut app.settings.hover_picker, hover);
    sync_picker(&mut app.settings.text_picker, text);
}

pub fn update(app: &mut App, msg: SettingsMsg) -> Task<cosmic::Action<Msg>> {
    match msg {
        SettingsMsg::Edge(i) => {
            if let Some(edge) = EDGES.get(i) {
                write(app, "edge", edge);
            }
        }
        SettingsMsg::Size(v) => write(app, "size", v),
        SettingsMsg::Fill(v) => write(app, "fill", v),
        SettingsMsg::Autohide(v) => write(app, "autohide", v),
        SettingsMsg::Clip(i) => {
            if let Some(clip) = CLIPS.get(i) {
                write(app, "clip", clip);
            }
        }
        SettingsMsg::RoundEdgeCorners(v) => write(app, "round_edge_corners", v),
        SettingsMsg::MediaEnabled(v) => write(app, "media_enabled", v),
        SettingsMsg::MediaPosition(i) => {
            if let Some(pos) = MEDIA_POSITIONS.get(i) {
                write(app, "media_position", pos);
            }
        }
        SettingsMsg::PreferredPlayer(v) => write(app, "preferred_player", v),
        SettingsMsg::MediaVolumeScroll(v) => write(app, "media_volume_scroll", v),
        SettingsMsg::OverrideBg(custom) => {
            let value = custom.then(|| {
                to_rgba(
                    app.settings
                        .bg_picker
                        .get_applied_color()
                        .unwrap_or_else(|| {
                            crate::view::panel_bg_color(
                                &cosmic::theme::active(),
                                &app.panel_theme,
                            )
                        }),
                )
            });
            write(app, "bg_color", value);
        }
        SettingsMsg::BgPicker(u) => {
            let commit = is_commit(&u);
            let task = app.settings.bg_picker.update::<cosmic::Action<Msg>>(u);
            if commit && let Some(c) = app.settings.bg_picker.get_applied_color() {
                write(app, "bg_color", Some(to_rgba(c)));
            }
            return task;
        }
        SettingsMsg::OverrideBlur(custom) => {
            write(app, "blur", custom.then(|| app.blur_enabled()));
        }
        SettingsMsg::Blur(v) => write(app, "blur", Some(v)),
        SettingsMsg::OverrideBarRadius(custom) => {
            write(
                app,
                "bar_radius",
                custom.then(|| app.panel_theme.border_radius),
            );
        }
        SettingsMsg::BarRadius(v) => write(app, "bar_radius", Some(v)),
        SettingsMsg::OverrideActiveBorderColor(custom) => {
            let value = custom.then(|| {
                to_rgb(
                    app.settings
                        .active_border_picker
                        .get_applied_color()
                        .unwrap_or_else(|| cosmic::theme::active().cosmic().accent.base.into()),
                )
            });
            write(app, "active_border_color", value);
        }
        SettingsMsg::ActiveBorderPicker(u) => {
            let commit = is_commit(&u);
            let task = app
                .settings
                .active_border_picker
                .update::<cosmic::Action<Msg>>(u);
            if commit && let Some(c) = app.settings.active_border_picker.get_applied_color() {
                write(app, "active_border_color", Some(to_rgb(c)));
            }
            return task;
        }
        SettingsMsg::OverrideActiveBorderWidth(custom) => {
            write(app, "active_border_width", custom.then_some(4.0f32));
        }
        SettingsMsg::ActiveBorderWidth(v) => write(app, "active_border_width", Some(v as f32)),
        SettingsMsg::OverrideHover(custom) => {
            let value = custom.then(|| {
                to_rgba(
                    app.settings
                        .hover_picker
                        .get_applied_color()
                        .unwrap_or_else(|| cosmic::theme::active().cosmic().button.base.into()),
                )
            });
            write(app, "hover_color", value);
        }
        SettingsMsg::HoverPicker(u) => {
            let commit = is_commit(&u);
            let task = app.settings.hover_picker.update::<cosmic::Action<Msg>>(u);
            if commit && let Some(c) = app.settings.hover_picker.get_applied_color() {
                write(app, "hover_color", Some(to_rgba(c)));
            }
            return task;
        }
        SettingsMsg::OverrideItemRadius(custom) => {
            // Matches the default item radius formula in view.rs
            let value = custom.then(|| {
                let x = cosmic::theme::active().cosmic().corner_radii.radius_s[0];
                (if x < 4.0 { x } else { x + 4.0 }) as u32
            });
            write(app, "item_radius", value);
        }
        SettingsMsg::ItemRadius(v) => write(app, "item_radius", Some(v)),
        SettingsMsg::OverrideText(custom) => {
            let value = custom.then(|| {
                to_rgb(
                    app.settings
                        .text_picker
                        .get_applied_color()
                        .unwrap_or_else(|| cosmic::theme::active().cosmic().on_bg_color().into()),
                )
            });
            write(app, "text_color", value);
        }
        SettingsMsg::TextPicker(u) => {
            let commit = is_commit(&u);
            let task = app.settings.text_picker.update::<cosmic::Action<Msg>>(u);
            if commit && let Some(c) = app.settings.text_picker.get_applied_color() {
                write(app, "text_color", Some(to_rgb(c)));
            }
            return task;
        }
        SettingsMsg::ResetAll => {
            write(app, "bg_color", None::<[f32; 4]>);
            write(app, "blur", None::<bool>);
            write(app, "bar_radius", None::<u32>);
            write(app, "active_border_color", None::<[f32; 3]>);
            write(app, "active_border_width", None::<f32>);
            write(app, "hover_color", None::<[f32; 4]>);
            write(app, "item_radius", None::<u32>);
            write(app, "text_color", None::<[f32; 3]>);
        }
    }
    Task::none()
}

fn msg(m: SettingsMsg) -> Msg {
    Msg::Settings(m)
}

fn bg_picker_msg(u: ColorPickerUpdate) -> Msg {
    Msg::Settings(SettingsMsg::BgPicker(u))
}

fn active_border_picker_msg(u: ColorPickerUpdate) -> Msg {
    Msg::Settings(SettingsMsg::ActiveBorderPicker(u))
}

fn hover_picker_msg(u: ColorPickerUpdate) -> Msg {
    Msg::Settings(SettingsMsg::HoverPicker(u))
}

fn text_picker_msg(u: ColorPickerUpdate) -> Msg {
    Msg::Settings(SettingsMsg::TextPicker(u))
}

fn override_row<'a>(
    label: &'a str,
    is_custom: bool,
    on_toggle: fn(bool) -> SettingsMsg,
    control: Option<Element<'a, Msg>>,
) -> Element<'a, Msg> {
    let mut row = widget::row::with_capacity(3)
        .spacing(12.0)
        .align_y(iced::Alignment::Center)
        .push(
            widget::toggler(is_custom)
                .label("Custom".to_string())
                .on_toggle(move |v| msg(on_toggle(v))),
        );
    if let Some(control) = control {
        row = row.push(control);
    }
    settings::item(label, row).into()
}

fn color_row<'a>(
    label: &'a str,
    override_value: bool,
    picker: &'a ColorPickerModel,
    on_toggle: fn(bool) -> SettingsMsg,
    on_picker: fn(ColorPickerUpdate) -> Msg,
) -> Vec<Element<'a, Msg>> {
    let control = override_value.then(|| {
        picker
            .picker_button(on_picker, None)
            .width(Length::Fixed(48.0))
            .height(Length::Fixed(24.0))
            .into()
    });
    let mut elements = vec![override_row(label, override_value, on_toggle, control)];
    if override_value && picker.get_is_active() {
        elements.push(
            picker
                .builder(on_picker)
                .reset_label("Reset")
                .save_label("Apply")
                .cancel_label("Cancel")
                .build("Recent colors", "Copy to clipboard", "Copied")
                .into(),
        );
    }
    elements
}

fn slider_row<'a>(
    label: &'a str,
    is_custom: bool,
    value: u32,
    range: std::ops::RangeInclusive<u32>,
    on_toggle: fn(bool) -> SettingsMsg,
    on_change: fn(u32) -> SettingsMsg,
) -> Element<'a, Msg> {
    let control = is_custom.then(|| {
        widget::row::with_capacity(2)
            .spacing(8.0)
            .align_y(iced::Alignment::Center)
            .push(
                widget::slider(range, value, move |v| msg(on_change(v)))
                    .width(Length::Fixed(160.0)),
            )
            .push(widget::text::body(format!("{value}")))
            .into()
    });
    override_row(label, is_custom, on_toggle, control)
}

pub fn view(app: &App) -> Element<'_, Msg> {
    let cfg = &app.config;
    let edge_idx = EDGES.iter().position(|e| *e == cfg.edge);

    // Layer surfaces have no compositor decorations; provide our own header
    let header = widget::row::with_capacity(3)
        .align_y(iced::Alignment::Center)
        .push(widget::text::title3("Workspaces Bar Settings"))
        .push(widget::Space::new().width(Length::Fill))
        .push(
            widget::button::icon(widget::icon::from_name("window-close-symbolic"))
                .on_press(Msg::CloseSettings),
        );

    let layout = settings::section()
        .title("Layout")
        .add(settings::item(
            "Position on screen",
            widget::dropdown(EDGE_LABELS, edge_idx, |i| msg(SettingsMsg::Edge(i))),
        ))
        .add(settings::item(
            "Size",
            widget::row::with_capacity(2)
                .spacing(8.0)
                .align_y(iced::Alignment::Center)
                .push(
                    widget::slider(64..=512u32, cfg.size, |v| msg(SettingsMsg::Size(v)))
                        .width(Length::Fixed(200.0)),
                )
                .push(widget::text::body(format!("{} px", cfg.size))),
        ))
        .add(
            settings::item::builder("Extend to edges of screen")
                .toggler(cfg.fill, |v| msg(SettingsMsg::Fill(v))),
        )
        .add(
            settings::item::builder("Automatically hide the bar")
                .toggler(cfg.autohide, |v| msg(SettingsMsg::Autohide(v))),
        )
        .add(
            settings::item::builder("Hide bars in previews")
                .description("Crop reserved bar areas out of workspace previews")
                .control(widget::dropdown(
                    CLIP_LABELS,
                    CLIPS.iter().position(|c| *c == cfg.clip),
                    |i| msg(SettingsMsg::Clip(i)),
                )),
        );

    let media = settings::section()
        .title("Media controls")
        .add(
            settings::item::builder("Show media controls")
                .description("Previous/play-pause/next, title and album art for the active player")
                .toggler(cfg.media_enabled, |v| msg(SettingsMsg::MediaEnabled(v))),
        )
        .add_maybe(cfg.media_enabled.then(|| {
            settings::item(
                "Position",
                widget::dropdown(
                    MEDIA_POSITION_LABELS,
                    MEDIA_POSITIONS.iter().position(|p| *p == cfg.media_position),
                    |i| msg(SettingsMsg::MediaPosition(i)),
                ),
            )
        }))
        .add_maybe(cfg.media_enabled.then(|| {
            settings::item(
                "Preferred player",
                widget::text_input("Automatic", &cfg.preferred_player)
                    .on_input(|v| msg(SettingsMsg::PreferredPlayer(v)))
                    .width(Length::Fixed(200.0)),
            )
        }))
        .add_maybe(cfg.media_enabled.then(|| {
            settings::item::builder("Adjust volume by scrolling")
                .description("Scroll over the media controls to change the active player's volume")
                .toggler(cfg.media_volume_scroll, |v| {
                    msg(SettingsMsg::MediaVolumeScroll(v))
                })
        }));

    let mut appearance = settings::section()
        .title("Appearance")
        .add(widget::text::caption(
            "With Custom off, the bar follows the COSMIC panel style",
        ));
    for el in color_row(
        "Background color",
        cfg.bg_color.is_some(),
        &app.settings.bg_picker,
        SettingsMsg::OverrideBg,
        bg_picker_msg,
    ) {
        appearance = appearance.add(el);
    }
    appearance = appearance.add(override_row(
        "Blur",
        cfg.blur.is_some(),
        SettingsMsg::OverrideBlur,
        cfg.blur.map(|b| {
            widget::toggler(b)
                .label("Enabled".to_string())
                .on_toggle(|v| msg(SettingsMsg::Blur(v)))
                .into()
        }),
    ));
    appearance = appearance.add(slider_row(
        "Bar corner radius",
        cfg.bar_radius.is_some(),
        cfg.bar_radius.unwrap_or(0),
        0..=64,
        SettingsMsg::OverrideBarRadius,
        SettingsMsg::BarRadius,
    ));
    appearance = appearance.add(
        settings::item::builder("Round corners at screen edge")
            .description("Turn off for square corners where the bar meets the edge")
            .toggler(cfg.round_edge_corners, |v| {
                msg(SettingsMsg::RoundEdgeCorners(v))
            }),
    );
    for el in color_row(
        "Active workspace border",
        cfg.active_border_color.is_some(),
        &app.settings.active_border_picker,
        SettingsMsg::OverrideActiveBorderColor,
        active_border_picker_msg,
    ) {
        appearance = appearance.add(el);
    }
    appearance = appearance.add(slider_row(
        "Active border width",
        cfg.active_border_width.is_some(),
        cfg.active_border_width.unwrap_or(4.0) as u32,
        1..=16,
        SettingsMsg::OverrideActiveBorderWidth,
        SettingsMsg::ActiveBorderWidth,
    ));
    for el in color_row(
        "Hover highlight",
        cfg.hover_color.is_some(),
        &app.settings.hover_picker,
        SettingsMsg::OverrideHover,
        hover_picker_msg,
    ) {
        appearance = appearance.add(el);
    }
    appearance = appearance.add(slider_row(
        "Item corner radius",
        cfg.item_radius.is_some(),
        cfg.item_radius.unwrap_or(8),
        0..=64,
        SettingsMsg::OverrideItemRadius,
        SettingsMsg::ItemRadius,
    ));
    for el in color_row(
        "Label text color",
        cfg.text_color.is_some(),
        &app.settings.text_picker,
        SettingsMsg::OverrideText,
        text_picker_msg,
    ) {
        appearance = appearance.add(el);
    }

    let reset = widget::button::standard("Reset all to follow top bar")
        .on_press(msg(SettingsMsg::ResetAll));

    // Windows other than the main one get no default background
    widget::container(widget::scrollable(
        widget::container(
            settings::view_column(vec![
                header.into(),
                layout.into(),
                media.into(),
                appearance.into(),
                reset.into(),
            ])
            .max_width(580.0)
            .padding(24.0),
        )
        .center_x(Length::Fill),
    ))
    .class(cosmic::theme::Container::WindowBackground)
    .width(Length::Fill)
    .height(Length::Fill)
    .into()
}
