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
const APP_IFACE: &str = "org.mpris.MediaPlayer2";
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

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum ArtSource {
    Local(std::path::PathBuf),
    Remote(String),
}

#[derive(Clone, Debug, PartialEq)]
pub struct PlayerState {
    pub bus_name: String,
    pub status: PlaybackStatus,
    pub title: String,
    pub artist: String,
    pub art: Option<ArtSource>,
    // MPRIS `DesktopEntry`, or a guess from the bus name if unsupported
    pub desktop_entry: String,
    pub can_raise: bool,
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

static PREFERRED_PLAYER: std::sync::OnceLock<std::sync::Mutex<String>> = std::sync::OnceLock::new();

/// Set (or clear, with an empty string) the user's preferred player match
pub fn set_preferred(pattern: String) {
    *PREFERRED_PLAYER
        .get_or_init(|| std::sync::Mutex::new(String::new()))
        .lock()
        .unwrap() = pattern;
}

fn preferred() -> String {
    PREFERRED_PLAYER
        .get_or_init(|| std::sync::Mutex::new(String::new()))
        .lock()
        .unwrap()
        .clone()
}

fn matches_preferred(candidate: &str, pattern: &str) -> bool {
    !pattern.is_empty() && candidate.to_lowercase().contains(&pattern.to_lowercase())
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

// Prefer an explicit user override if present and available; else whichever
// candidate is actively playing; else the first one found
async fn pick_playing(conn: &Connection, candidates: Vec<String>) -> Option<String> {
    let pattern = preferred();
    if !pattern.is_empty()
        && let Some(name) = candidates.iter().find(|n| matches_preferred(n, &pattern))
    {
        return Some(name.clone());
    }
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

    // Static for the lifetime of this player process; fetched once
    let desktop_entry = match props.get(APP_IFACE.try_into().unwrap(), "DesktopEntry").await {
        Ok(v) => String::try_from(v).ok().filter(|s| !s.is_empty()),
        Err(_) => None,
    }
    .unwrap_or_else(|| guess_app_id(name));
    let can_raise = props
        .get(APP_IFACE.try_into().unwrap(), "CanRaise")
        .await
        .ok()
        .and_then(|v| bool::try_from(v).ok())
        .unwrap_or(false);

    let all = match props.get_all(PLAYER_IFACE.try_into().unwrap()).await {
        Ok(a) => a,
        Err(err) => return PlayerOutcome::Error(err.into()),
    };
    let mut playing = get_str(&all, "PlaybackStatus").as_deref() == Some("Playing");
    if sender
        .send(Event::Player(Some(parse_state(name, &all, &desktop_entry, can_raise))))
        .await
        .is_err()
    {
        return PlayerOutcome::Gone;
    }

    loop {
        // Re-evaluate while idle, or continuously if the user has an explicit
        // preference that isn't the currently-selected player
        let pattern = preferred();
        let needs_recheck = !playing || (!pattern.is_empty() && !matches_preferred(name, &pattern));
        let recheck = tokio::time::sleep(RECHECK_INTERVAL);
        tokio::select! {
            _ = recheck, if needs_recheck => {
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
                    .send(Event::Player(Some(parse_state(name, &all, &desktop_entry, can_raise))))
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

// Fallback when a player doesn't implement DesktopEntry (e.g. chromium):
// take the first dotted segment after the mpris prefix as a guess at the app id
fn guess_app_id(bus_name: &str) -> String {
    bus_name
        .strip_prefix(MPRIS_PREFIX)
        .unwrap_or(bus_name)
        .split('.')
        .next()
        .unwrap_or(bus_name)
        .to_string()
}

fn parse_state(
    name: &str,
    props: &HashMap<String, OwnedValue>,
    desktop_entry: &str,
    can_raise: bool,
) -> PlayerState {
    let status = match get_str(props, "PlaybackStatus").as_deref() {
        Some("Playing") => PlaybackStatus::Playing,
        Some("Paused") => PlaybackStatus::Paused,
        _ => PlaybackStatus::Stopped,
    };

    let mut title = String::new();
    let mut artist = String::new();
    let mut art = None;
    if let Some(metadata) = props.get("Metadata")
        && let Ok(metadata) = HashMap::<String, OwnedValue>::try_from(metadata.clone())
    {
        title = get_str(&metadata, "xesam:title").unwrap_or_default();
        artist = metadata
            .get("xesam:artist")
            .and_then(|v| <Vec<String>>::try_from(v.clone()).ok())
            .and_then(|v| v.into_iter().next())
            .unwrap_or_default();
        art = metadata
            .get("mpris:artUrl")
            .and_then(|v| String::try_from(v.clone()).ok())
            .and_then(|url| {
                if let Some(path) = url.strip_prefix("file://") {
                    Some(ArtSource::Local(std::path::PathBuf::from(path)))
                } else if url.starts_with("http://") || url.starts_with("https://") {
                    Some(ArtSource::Remote(url))
                } else {
                    None
                }
            });
    }

    PlayerState {
        bus_name: name.to_string(),
        status,
        title,
        artist,
        art,
        desktop_entry: desktop_entry.to_string(),
        can_raise,
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

pub async fn raise(bus_name: String) {
    let result: zbus::Result<()> = async {
        let conn = Connection::session().await?;
        conn.call_method(Some(bus_name), PLAYER_PATH, Some(APP_IFACE), "Raise", &())
            .await?;
        Ok(())
    }
    .await;
    if let Err(err) = result {
        log::warn!("failed to raise media player: {err}");
    }
}

/// Nudge playback volume up/down by `delta` (e.g. 0.05 for +5%), clamped to [0, 1]
/// Nudge playback volume up/down by `delta` (e.g. 0.05 for +5%), clamped to
/// [0, 1]; returns the new value on success, for showing a brief indicator
pub async fn adjust_volume(bus_name: String, delta: f64) -> Option<f64> {
    let result: zbus::Result<f64> = async {
        let conn = Connection::session().await?;
        let props = fdo::PropertiesProxy::new(&conn, bus_name, PLAYER_PATH).await?;
        let current: f64 = props
            .get(PLAYER_IFACE.try_into().unwrap(), "Volume")
            .await
            .ok()
            .and_then(|v| f64::try_from(v).ok())
            .unwrap_or(1.0);
        let new_volume = (current + delta).clamp(0.0, 1.0);
        props
            .set(PLAYER_IFACE.try_into().unwrap(), "Volume", new_volume.into())
            .await?;
        Ok(new_volume)
    }
    .await;
    match result {
        Ok(v) => Some(v),
        Err(err) => {
            log::warn!("failed to adjust media volume: {err}");
            None
        }
    }
}

/// Max album art response size, to bound memory use against a huge/misbehaving server
const MAX_ART_BYTES: usize = 10 * 1024 * 1024;
/// Retry a transient fetch failure (timeout, connection reset) this many times
const ART_FETCH_RETRIES: u32 = 2;
/// Cap on-disk album art cache size; oldest entries are pruned past this
const ART_CACHE_MAX_BYTES: u64 = 50 * 1024 * 1024;

/// Fetch remote album art bytes, retrying transient failures with backoff
pub async fn fetch_art(url: String) -> Option<Vec<u8>> {
    for attempt in 0..=ART_FETCH_RETRIES {
        if attempt > 0 {
            tokio::time::sleep(Duration::from_secs(attempt as u64)).await;
        }
        match fetch_art_once(&url).await {
            Ok(bytes) => return Some(bytes),
            Err(err) => log::warn!("album art fetch attempt {attempt} for {url} failed: {err}"),
        }
    }
    None
}

async fn fetch_art_once(url: &str) -> Result<Vec<u8>, String> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .map_err(|e| e.to_string())?;
    let resp = client
        .get(url)
        .send()
        .await
        .map_err(|e| e.to_string())?
        .error_for_status()
        .map_err(|e| e.to_string())?;
    if resp.content_length().is_some_and(|len| len > MAX_ART_BYTES as u64) {
        return Err("response exceeds size limit".to_string());
    }
    let bytes = resp.bytes().await.map_err(|e| e.to_string())?;
    if bytes.len() > MAX_ART_BYTES {
        return Err("response exceeds size limit".to_string());
    }
    Ok(bytes.to_vec())
}

fn art_cache_dir() -> Option<std::path::PathBuf> {
    let base = if let Ok(v) = std::env::var("XDG_CACHE_HOME") {
        std::path::PathBuf::from(v)
    } else {
        std::path::PathBuf::from(std::env::var("HOME").ok()?).join(".cache")
    };
    Some(base.join("cosmic-ext-workspaces-bar").join("art"))
}

// Stable, dependency-free hash for cache filenames (not security-sensitive)
fn hash_url(url: &str) -> String {
    let mut hash: u64 = 0xcbf29ce484222325;
    for b in url.bytes() {
        hash ^= u64::from(b);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("{hash:016x}")
}

/// Fetch remote album art, transparently caching successful downloads on disk
pub async fn fetch_art_cached(url: String) -> Option<Vec<u8>> {
    let cache_path = art_cache_dir().map(|d| d.join(hash_url(&url)));
    if let Some(path) = &cache_path
        && let Ok(bytes) = tokio::fs::read(path).await
        && !bytes.is_empty()
    {
        return Some(bytes);
    }

    let bytes = fetch_art(url).await?;

    if let Some(path) = cache_path {
        let write_bytes = bytes.clone();
        tokio::spawn(async move {
            if let Some(dir) = path.parent() {
                let _ = tokio::fs::create_dir_all(dir).await;
            }
            if tokio::fs::write(&path, &write_bytes).await.is_ok()
                && let Some(dir) = path.parent().map(std::path::Path::to_path_buf)
            {
                tokio::task::spawn_blocking(move || prune_art_cache(&dir));
            }
        });
    }

    Some(bytes)
}

// Evict oldest-modified files once the cache exceeds ART_CACHE_MAX_BYTES
fn prune_art_cache(dir: &std::path::Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut files: Vec<(std::path::PathBuf, u64, std::time::SystemTime)> = entries
        .flatten()
        .filter_map(|e| {
            let meta = e.metadata().ok()?;
            if !meta.is_file() {
                return None;
            }
            Some((e.path(), meta.len(), meta.modified().ok()?))
        })
        .collect();
    let total: u64 = files.iter().map(|(_, size, _)| size).sum();
    if total <= ART_CACHE_MAX_BYTES {
        return;
    }
    files.sort_by_key(|(_, _, modified)| *modified);
    let mut over = total - ART_CACHE_MAX_BYTES;
    for (path, size, _) in files {
        if over == 0 {
            break;
        }
        if std::fs::remove_file(&path).is_ok() {
            over = over.saturating_sub(size);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_url_is_stable_and_distinct() {
        let a = hash_url("https://example.com/a.jpg");
        let b = hash_url("https://example.com/b.jpg");
        assert_eq!(a, hash_url("https://example.com/a.jpg"));
        assert_ne!(a, b);
    }

    // Exercises the real network fetch + disk cache + retry-free happy path
    // against a stable public test image, independent of any MPRIS player.
    // Ignored by default since it depends on network access; run explicitly
    // with `cargo test -- --ignored` to verify after touching this pipeline
    #[ignore]
    #[tokio::test]
    async fn fetch_art_cached_downloads_and_caches() {
        let dir = std::env::temp_dir().join(format!("workspaces-bar-art-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        // SAFETY: single-threaded test process, no concurrent env access
        unsafe {
            std::env::set_var("XDG_CACHE_HOME", &dir);
        }

        let url = "https://httpbin.org/image/png".to_string();
        let bytes = fetch_art_cached(url.clone())
            .await
            .expect("first fetch should succeed over the network");
        assert!(!bytes.is_empty());

        let cache_path = art_cache_dir().unwrap().join(hash_url(&url));
        // Cache write is dispatched on a detached task; give it a moment
        for _ in 0..20 {
            if cache_path.exists() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        assert!(cache_path.exists(), "expected art to be written to disk cache");

        // Second call should be served from disk without hitting the network
        std::fs::remove_file(&cache_path).unwrap_or(());
        std::fs::write(&cache_path, b"cached-marker").unwrap();
        let cached = fetch_art_cached(url).await.unwrap();
        assert_eq!(cached, b"cached-marker");

        let _ = std::fs::remove_dir_all(&dir);
    }
}

