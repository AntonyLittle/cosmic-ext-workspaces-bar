// SPDX-License-Identifier: GPL-3.0-only

//! Detects resume-from-sleep and session unlock via logind (system D-Bus),
//! to force a prompt workspace thumbnail refresh: screencopy while the
//! session is locked captures the lock screen overlay (composited into
//! every workspace's frame for that output), and suspend can otherwise
//! leave capture sessions stale until our own adaptive backoff catches up.

use std::time::Duration;

use cosmic::iced::futures::channel::mpsc;
use cosmic::iced::futures::{SinkExt, StreamExt, stream};
use zbus::Connection;

#[derive(Clone, Copy, Debug)]
pub enum Event {
    Resumed,
}

#[zbus::proxy(
    interface = "org.freedesktop.login1.Manager",
    default_service = "org.freedesktop.login1",
    default_path = "/org/freedesktop/login1"
)]
trait Login1Manager {
    #[zbus(signal)]
    fn prepare_for_sleep(&self, start: bool) -> zbus::Result<()>;

    fn get_session(&self, session_id: &str) -> zbus::Result<zbus::zvariant::OwnedObjectPath>;
}

#[zbus::proxy(
    interface = "org.freedesktop.login1.Session",
    default_service = "org.freedesktop.login1"
)]
trait Login1Session {
    #[zbus(signal)]
    fn unlock(&self) -> zbus::Result<()>;
}

pub fn subscription() -> cosmic::iced::Subscription<Event> {
    cosmic::iced::Subscription::run(|| cosmic::iced::stream::channel(4, run))
}

async fn run(mut sender: mpsc::Sender<Event>) {
    loop {
        if let Err(err) = watch(&mut sender).await {
            log::warn!("power backend error, retrying: {err}");
        }
        tokio::time::sleep(Duration::from_secs(5)).await;
    }
}

async fn watch(sender: &mut mpsc::Sender<Event>) -> zbus::Result<()> {
    let conn = Connection::system().await?;
    let manager = Login1ManagerProxy::new(&conn).await?;

    let session = session_proxy(&conn).await;

    let resumed = manager
        .receive_prepare_for_sleep()
        .await?
        .filter_map(|signal| async move { signal.args().ok().filter(|a| !a.start).map(|_| ()) });

    let unlocked: std::pin::Pin<Box<dyn cosmic::iced::futures::Stream<Item = ()> + Send>> =
        match session {
            Some(session) => Box::pin(session.receive_unlock().await?.map(|_| ())),
            None => Box::pin(stream::pending()),
        };

    let mut events = std::pin::pin!(stream::select(resumed, unlocked));
    while events.next().await.is_some() {
        let _ = sender.send(Event::Resumed).await;
    }
    Ok(())
}

// Resolves a proxy for the current login session, if logind exposes one
// (e.g. absent without a proper systemd-logind session, or on other inits)
async fn session_proxy(conn: &Connection) -> Option<Login1SessionProxy<'static>> {
    let session_id = std::env::var("XDG_SESSION_ID").ok()?;
    let manager = Login1ManagerProxy::new(conn).await.ok()?;
    let path = manager.get_session(&session_id).await.ok()?;
    Login1SessionProxy::builder(conn)
        .path(path)
        .ok()?
        .build()
        .await
        .ok()
}
