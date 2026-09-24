// SPDX-License-Identifier: GPL-3.0-only

//! MPRIS2 media player integration (previous/play-pause/next, title/artist, art).

use std::collections::HashMap;
use std::time::Duration;

use cosmic::iced::futures::channel::mpsc;
use cosmic::iced::futures::{SinkExt, StreamExt};
use zbus::zvariant::OwnedValue;
use zbus::{Connection, fdo};

const MPRIS_PREFIX: &str = "org.mpris.MediaPlayer2.";
const PLAYER_PATH: &str = "/org/mpris/MediaPlayer2";
const PLAYER_IFACE: &str = "org.mpris.MediaPlayer2.Player";
/// While the current player isn't actively playing, periodically check
/// whether a different player has started, since MPRIS has no "now playing
/// changed" signal covering every player at once
const RECHECK_INTERVAL: Duration = Duration::from_secs(3);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlaybackStatus {
    Playing,
    Paused,
    Stopped,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PlayerState {
    pub bus_name: String,
    pub status: PlaybackStatus,
    pub title: String,
    pub artist: String,
    pub art_path: Option<std::path::PathBuf>,
    pub can_go_next: bool,
    pub can_go_previous: bool,
    pub can_play_pause: bool,
}

#[derive(Clone, Debug)]
pub enum Event {
    Player(Option<PlayerState>),
}

#[derive(Clone, Copy, Debug)]
pub enum Control {
    Previous,
    PlayPause,
    Next,
}

pub fn subscription() -> cosmic::iced::Subscription<Event> {
    cosmic::iced::Subscription::run(|| cosmic::iced::stream::channel(8, run))
}

async fn run(mut sender: mpsc::Sender<Event>) {
    loop {
        if let Err(err) = watch(&mut sender).await {
            log::warn!("media backend error, retrying: {err}");
        }
        let _ = sender.send(Event::Player(None)).await;
        tokio::time::sleep(Duration::from_secs(5)).await;
    }
}

async fn watch(sender: &mut mpsc::Sender<Event>) -> zbus::Result<()> {
    let conn = Connection::session().await?;
    let dbus = fdo::DBusProxy::new(&conn).await?;

    let mut owner_changes = dbus.receive_name_owner_changed().await?;

    // Seed with any players already running, preferring one that's playing
    let mut candidates = Vec::new();
    for name in dbus.list_names().await? {
        if name.starts_with(MPRIS_PREFIX) {
            candidates.push(name.to_string());
        }
    }
    let mut current = pick_playing(&conn, candidates).await;

    loop {
        let Some(name) = current.clone() else {
            let _ = sender.send(Event::Player(None)).await;
            // Wait for a player to appear
            loop {
                let Some(sig) = owner_changes.next().await else {
                    return Ok(());
                };
                let Ok(args) = sig.args() else { continue };
                if args.name().starts_with(MPRIS_PREFIX) && args.new_owner().is_some() {
                    current = Some(args.name().to_string());
                    break;
                }
            }
            continue;
        };

        match watch_player(&conn, &name, sender, &mut owner_changes).await {
            PlayerOutcome::Gone => current = None,
            PlayerOutcome::Switch(new_name) => current = Some(new_name),
            PlayerOutcome::Error(err) => {
                log::warn!("lost connection to media player {name}: {err}");
                return Err(err);
            }
        }
    }
}

enum PlayerOutcome {
    Gone,
    Switch(String),
    Error(zbus::Error),
}

// Any org.mpris.MediaPlayer2.* bus name currently on the session bus
async fn mpris_candidates(conn: &Connection) -> zbus::Result<Vec<String>> {
    let dbus = fdo::DBusProxy::new(conn).await?;
    Ok(dbus
        .list_names()
        .await?
        .into_iter()
        .filter(|n| n.starts_with(MPRIS_PREFIX))
        .map(|n| n.to_string())
        .collect())
}

// Prefer whichever candidate is actively playing; else the first one found
async fn pick_playing(conn: &Connection, candidates: Vec<String>) -> Option<String> {
    let mut fallback = None;
    for name in candidates {
        if let Ok(props) = fdo::PropertiesProxy::new(conn, name.clone(), PLAYER_PATH).await
            && let Ok(status) = props
                .get(PLAYER_IFACE.try_into().unwrap(), "PlaybackStatus")
                .await
            && String::try_from(status).ok().as_deref() == Some("Playing")
        {
            return Some(name);
        }
        fallback.get_or_insert(name);
    }
    fallback
}

async fn watch_player(
    conn: &Connection,
    name: &str,
    sender: &mut mpsc::Sender<Event>,
    owner_changes: &mut fdo::NameOwnerChangedStream,
) -> PlayerOutcome {
    let props = match fdo::PropertiesProxy::new(conn, name.to_owned(), PLAYER_PATH).await {
        Ok(p) => p,
        Err(err) => return PlayerOutcome::Error(err),
    };
    let mut changes = match props.receive_properties_changed().await {
        Ok(s) => s,
        Err(err) => return PlayerOutcome::Error(err),
    };

    let all = match props.get_all(PLAYER_IFACE.try_into().unwrap()).await {
        Ok(a) => a,
        Err(err) => return PlayerOutcome::Error(err.into()),
    };
    let mut playing = get_str(&all, "PlaybackStatus").as_deref() == Some("Playing");
    if sender
        .send(Event::Player(Some(parse_state(name, &all))))
        .await
        .is_err()
    {
        return PlayerOutcome::Gone;
    }

    loop {
        let recheck = tokio::time::sleep(RECHECK_INTERVAL);
        tokio::select! {
            _ = recheck, if !playing => {
                if let Ok(candidates) = mpris_candidates(conn).await
                    && let Some(best) = pick_playing(conn, candidates).await
                    && best != name
                {
                    return PlayerOutcome::Switch(best);
                }
            }
            change = changes.next() => {
                let Some(change) = change else { return PlayerOutcome::Gone };
                let Ok(args) = change.args() else { continue };
                if args.interface_name() != PLAYER_IFACE {
                    continue;
                }
                let mut merged: HashMap<String, OwnedValue> = args
                    .changed_properties()
                    .iter()
                    .map(|(k, v)| (k.to_string(), v.try_to_owned().unwrap()))
                    .collect();
                // Re-fetch anything invalidated so we always have a full snapshot
                if !args.invalidated_properties().is_empty()
                    && let Ok(all) = props.get_all(PLAYER_IFACE.try_into().unwrap()).await
                {
                    for k in args.invalidated_properties().iter() {
                        if let Some(v) = all.get(*k) {
                            merged.insert(k.to_string(), v.clone());
                        }
                    }
                }
                let mut all = match props.get_all(PLAYER_IFACE.try_into().unwrap()).await {
                    Ok(a) => a,
                    Err(err) => return PlayerOutcome::Error(err.into()),
                };
                all.extend(merged);
                playing = get_str(&all, "PlaybackStatus").as_deref() == Some("Playing");
                if sender
                    .send(Event::Player(Some(parse_state(name, &all))))
                    .await
                    .is_err()
                {
                    return PlayerOutcome::Gone;
                }
            }
            owner_change = owner_changes.next() => {
                let Some(sig) = owner_change else { return PlayerOutcome::Gone };
                let Ok(args) = sig.args() else { continue };
                if args.name() == name && args.new_owner().is_none() {
                    return PlayerOutcome::Gone;
                }
            }
        }
    }
}

fn get_str(map: &HashMap<String, OwnedValue>, key: &str) -> Option<String> {
    map.get(key)
        .and_then(|v| String::try_from(v.clone()).ok())
}

fn get_bool(map: &HashMap<String, OwnedValue>, key: &str) -> bool {
    map.get(key)
        .and_then(|v| bool::try_from(v.clone()).ok())
        .unwrap_or(false)
}

fn parse_state(name: &str, props: &HashMap<String, OwnedValue>) -> PlayerState {
    let status = match get_str(props, "PlaybackStatus").as_deref() {
        Some("Playing") => PlaybackStatus::Playing,
        Some("Paused") => PlaybackStatus::Paused,
        _ => PlaybackStatus::Stopped,
    };

    let mut title = String::new();
    let mut artist = String::new();
    let mut art_path = None;
    if let Some(metadata) = props.get("Metadata")
        && let Ok(metadata) = HashMap::<String, OwnedValue>::try_from(metadata.clone())
    {
        title = get_str(&metadata, "xesam:title").unwrap_or_default();
        artist = metadata
            .get("xesam:artist")
            .and_then(|v| <Vec<String>>::try_from(v.clone()).ok())
            .and_then(|v| v.into_iter().next())
            .unwrap_or_default();
        art_path = metadata
            .get("mpris:artUrl")
            .and_then(|v| String::try_from(v.clone()).ok())
            .and_then(|url| url.strip_prefix("file://").map(str::to_owned))
            .map(std::path::PathBuf::from);
    }

    PlayerState {
        bus_name: name.to_string(),
        status,
        title,
        artist,
        art_path,
        can_go_next: get_bool(props, "CanGoNext"),
        can_go_previous: get_bool(props, "CanGoPrevious"),
        can_play_pause: get_bool(props, "CanPause") || get_bool(props, "CanPlay"),
    }
}

pub async fn send_control(bus_name: String, control: Control) {
    let method = match control {
        Control::Previous => "Previous",
        Control::PlayPause => "PlayPause",
        Control::Next => "Next",
    };
    let result: zbus::Result<()> = async {
        let conn = Connection::session().await?;
        conn.call_method(Some(bus_name), PLAYER_PATH, Some(PLAYER_IFACE), method, &())
            .await?;
        Ok(())
    }
    .await;
    if let Err(err) = result {
        log::warn!("media control {method} failed: {err}");
    }
}
