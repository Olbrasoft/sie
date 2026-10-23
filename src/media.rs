//! Observe MPRIS playback without intercepting keys or controlling any player.
use anyhow::{Context, Result, bail};
use futures_util::StreamExt;
use std::{
    collections::{HashMap, HashSet},
    time::Duration,
};
use tokio::sync::{mpsc, watch};
use zbus::{Connection, MatchRule, MessageStream, message::Type, zvariant::OwnedValue};

pub const PLAYER_PATH: &str = "/org/mpris/MediaPlayer2";
pub const PLAYER_INTERFACE: &str = "org.mpris.MediaPlayer2.Player";
const ROOT_INTERFACE: &str = "org.mpris.MediaPlayer2";
const PREFIX: &str = "org.mpris.MediaPlayer2.";

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Playback {
    #[default]
    Unavailable,
    Paused,
    Playing,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Status {
    pub playback: Playback,
    pub player: Option<String>,
    pub target: Option<String>,
}
impl Status {
    pub fn label(&self) -> String {
        match (&self.player, self.playback) {
            (Some(player), Playback::Playing) => format!("Média: {player} · Přehrává"),
            (Some(player), Playback::Paused) => {
                format!("Média: {player} · Pozastaveno / zastaveno")
            }
            _ => "Média: přehrávač není dostupný".into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Player {
    name: String,
    owner: String,
    identity: String,
    raw_status: String,
    can_control: bool,
    can_play: bool,
    can_pause: bool,
}
impl Player {
    fn playing(&self) -> bool {
        self.raw_status == "Playing"
    }
    fn indicator(&self) -> Playback {
        if !self.can_control {
            return Playback::Unavailable;
        }
        match self.raw_status.as_str() {
            "Playing" if self.can_pause => Playback::Playing,
            "Paused" | "Stopped" if self.can_play || self.can_pause => Playback::Paused,
            _ => Playback::Unavailable,
        }
    }
}

/// Model GNOME 48's persistent current-player and fallback ordering.
/// Focus changes never select a player. Native target observations can resolve
/// startup ambiguity when several players were already present before SIE.
#[derive(Default)]
struct Router {
    players: HashMap<String, Player>,
    order: Vec<String>,
}
impl Router {
    fn current_playing(&self) -> bool {
        self.order
            .first()
            .and_then(|name| self.players.get(name))
            .is_some_and(Player::playing)
    }
    fn appeared(&mut self, player: Player) {
        if self
            .players
            .get(&player.name)
            .is_some_and(|old| old.owner == player.owner)
        {
            self.changed(player);
            return;
        }
        self.remove(&player.name);
        let position = usize::from(self.current_playing());
        self.order.insert(position, player.name.clone());
        self.players.insert(player.name.clone(), player);
    }
    fn changed(&mut self, player: Player) {
        let name = player.name.clone();
        let promote =
            self.order.first() != Some(&name) && !self.current_playing() && player.playing();
        self.players.insert(name.clone(), player);
        if promote {
            self.select(&name);
        }
    }
    fn select(&mut self, name: &str) {
        if self.players.contains_key(name) {
            self.order.retain(|old| old != name);
            self.order.insert(0, name.into());
        }
    }
    fn observed_target(&mut self, destination: &str) {
        let name = if self.players.contains_key(destination) {
            Some(destination.to_owned())
        } else {
            let names: Vec<_> = self
                .players
                .values()
                .filter(|p| p.owner == destination)
                .map(|p| p.name.clone())
                .collect();
            (names.len() == 1).then(|| names[0].clone())
        };
        if let Some(name) = name {
            self.select(&name);
        }
    }
    fn remove(&mut self, name: &str) {
        self.order.retain(|old| old != name);
        self.players.remove(name);
    }
    fn status(&self) -> Status {
        match self.order.first().and_then(|name| self.players.get(name)) {
            Some(player) => Status {
                playback: player.indicator(),
                player: Some(player.identity.clone()),
                target: Some(player.name.clone()),
            },
            None => Status::default(),
        }
    }
}

async fn read_player(connection: &Connection, name: &str) -> Result<Player> {
    tokio::time::timeout(Duration::from_millis(700), async {
        let daemon = zbus::fdo::DBusProxy::new(connection).await?;
        let owner = daemon.get_name_owner(name.try_into()?).await?.to_string();
        let props = zbus::fdo::PropertiesProxy::builder(connection)
            .destination(name)?
            .path(PLAYER_PATH)?
            .build()
            .await?;
        let values = props.get_all(PLAYER_INTERFACE.try_into()?).await?;
        let root = props
            .get_all(ROOT_INTERFACE.try_into()?)
            .await
            .unwrap_or_default();
        let string = |values: &HashMap<String, OwnedValue>, key: &str| {
            values
                .get(key)
                .and_then(|v| <&str>::try_from(v).ok())
                .unwrap_or_default()
                .to_owned()
        };
        let flag = |key: &str| {
            values
                .get(key)
                .and_then(|v| bool::try_from(v).ok())
                .unwrap_or(false)
        };
        let identity = string(&root, "Identity");
        anyhow::Ok(Player {
            name: name.into(),
            owner,
            identity: if identity.is_empty() {
                "Přehrávač".into()
            } else {
                identity
            },
            raw_status: string(&values, "PlaybackStatus"),
            can_control: flag("CanControl"),
            can_play: flag("CanPlay"),
            can_pause: flag("CanPause"),
        })
    })
    .await
    .context("Media player did not respond")?
}

async fn reconcile(connection: &Connection, router: &mut Router) -> Result<()> {
    let daemon = zbus::fdo::DBusProxy::new(connection).await?;
    let names: HashSet<String> = daemon
        .list_names()
        .await?
        .into_iter()
        .map(|n| n.to_string())
        .filter(|n| n.starts_with(PREFIX))
        .collect();
    for old in router.order.clone() {
        if !names.contains(&old) {
            router.remove(&old);
        }
    }
    let mut discovered = Vec::new();
    for name in names {
        match read_player(connection, &name).await {
            Ok(player) => discovered.push(player),
            Err(_) => {
                if let Some(old) = router.players.get_mut(&name) {
                    old.raw_status.clear();
                }
            }
        }
    }
    // Connection serials give a stable initial approximation of appearance order.
    // GNOME's private pre-existing selection is synchronized on its next key call.
    discovered.sort_by_key(|p| {
        p.owner
            .rsplit('.')
            .next()
            .and_then(|n| n.parse::<u64>().ok())
            .unwrap_or(0)
    });
    for player in discovered {
        match router.players.get(&player.name) {
            Some(old) if old.owner == player.owner => {
                if *old != player {
                    router.changed(player);
                }
            }
            _ => router.appeared(player),
        }
    }
    Ok(())
}

async fn session(
    connection: &Connection,
    status: &watch::Sender<Status>,
    targets: &mut mpsc::Receiver<String>,
) -> Result<()> {
    let props_rule = MatchRule::builder()
        .msg_type(Type::Signal)
        .interface("org.freedesktop.DBus.Properties")?
        .member("PropertiesChanged")?
        .path(PLAYER_PATH)?
        .add_arg(PLAYER_INTERFACE)?
        .build();
    let owner_rule = MatchRule::builder()
        .msg_type(Type::Signal)
        .sender("org.freedesktop.DBus")?
        .interface("org.freedesktop.DBus")?
        .member("NameOwnerChanged")?
        .build();
    let mut properties = MessageStream::for_match_rule(props_rule, connection, Some(128)).await?;
    let mut owners = MessageStream::for_match_rule(owner_rule, connection, Some(64)).await?;
    let mut router = Router::default();
    reconcile(connection, &mut router).await?;
    status.send_replace(router.status());
    let mut refresh = tokio::time::interval(Duration::from_secs(2));
    refresh.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            _=status.closed()=>return Ok(()),
            Some(destination)=targets.recv()=>router.observed_target(&destination),
            message=owners.next()=>{
                let message=message.context("Media name-owner stream closed")??;
                let (name,old,new):(String,String,String)=message.body().deserialize()?;
                if !name.starts_with(PREFIX){continue;}
                if !old.is_empty(){router.remove(&name);}
                if !new.is_empty() && let Ok(player)=read_player(connection,&name).await {router.appeared(player);}
            },
            message=properties.next()=>{
                let message=message.context("Media property stream closed")??;
                let owner=message.header().sender().map(|n|n.as_str().to_owned()).unwrap_or_default();
                let names:Vec<_>=router.players.values().filter(|p|p.owner==owner).map(|p|p.name.clone()).collect();
                for name in names {
                    match read_player(connection,&name).await {
                        Ok(player)=>router.changed(player),
                        Err(_)=>if let Some(old)=router.players.get_mut(&name){old.raw_status.clear();},
                    }
                }
            },
            _=refresh.tick()=>reconcile(connection,&mut router).await?,
        }
        let new = router.status();
        status.send_if_modified(|old| {
            if *old != new {
                *old = new;
                true
            } else {
                false
            }
        });
    }
}

pub async fn watch(
    connection: Connection,
    status: watch::Sender<Status>,
    mut targets: mpsc::Receiver<String>,
) {
    while !status.is_closed() {
        if let Err(error) = session(&connection, &status, &mut targets).await {
            tracing::debug!(%error,"Media tracking unavailable; retrying");
            status.send_replace(Status::default());
        }
        if status.is_closed() {
            return;
        }
        tokio::time::sleep(Duration::from_secs(3)).await;
    }
}

/// A dedicated connection observes only zero-argument native PlayPause calls
/// from GNOME's media-key service. No key events, titles or media URLs are read.
/// Monitoring permission is optional; state tracking works without it.
pub struct NativeObserver {
    messages: MessageStream,
    _connection: Connection,
}

impl NativeObserver {
    pub async fn new(connection: Connection) -> Result<Self> {
        let rule = MatchRule::builder()
            .msg_type(Type::MethodCall)
            .sender("org.gnome.SettingsDaemon.MediaKeys")?
            .path(PLAYER_PATH)?
            .interface(PLAYER_INTERFACE)?
            .member("PlayPause")?
            .build();
        let messages = MessageStream::from(&connection);
        zbus::fdo::MonitoringProxy::new(&connection)
            .await?
            .become_monitor(&[rule], 0)
            .await?;
        Ok(Self {
            messages,
            _connection: connection,
        })
    }
    pub async fn run(self, targets: mpsc::Sender<String>) -> Result<()> {
        self.run_with_hint(targets, None).await
    }
    pub async fn run_with_hint(
        mut self,
        targets: mpsc::Sender<String>,
        hints: Option<watch::Sender<Option<crate::media_focus::NativeHint>>>,
    ) -> Result<()> {
        while let Some(message) = self.messages.next().await {
            let message = message?;
            let header = message.header();
            if header.message_type() != Type::MethodCall
                || header.interface().map(|i| i.as_str()) != Some(PLAYER_INTERFACE)
                || header.member().map(|m| m.as_str()) != Some("PlayPause")
            {
                continue;
            }
            if let Some(destination) = header.destination() {
                if let Some(hints) = &hints {
                    hints.send_replace(Some(crate::media_focus::NativeHint {
                        at: std::time::Instant::now(),
                        destination: destination.to_string(),
                    }));
                }
                let _ = targets.try_send(destination.to_string());
            }
            if targets.is_closed() {
                return Ok(());
            }
        }
        bail!("Native media target observation ended")
    }
}

pub async fn observe_native_target(
    connection: Connection,
    targets: mpsc::Sender<String>,
) -> Result<()> {
    NativeObserver::new(connection).await?.run(targets).await
}

#[cfg(test)]
mod tests {
    use super::*;
    fn player(name: &str, state: &str) -> Player {
        Player {
            name: format!("{PREFIX}{name}"),
            owner: format!(":1.{}", if name == "Chrome" { 1 } else { 2 }),
            identity: name.into(),
            raw_status: state.into(),
            can_control: true,
            can_play: true,
            can_pause: true,
        }
    }
    #[test]
    fn playback_colors_follow_capabilities_and_stopped_is_red() {
        for state in ["Paused", "Stopped"] {
            assert_eq!(player("VLC", state).indicator(), Playback::Paused);
        }
        assert_eq!(player("VLC", "Playing").indicator(), Playback::Playing);
        let mut p = player("VLC", "Playing");
        p.can_pause = false;
        assert_eq!(p.indicator(), Playback::Unavailable);
        p = player("VLC", "Paused");
        p.can_control = false;
        assert_eq!(p.indicator(), Playback::Unavailable);
        p.can_control = true;
        p.can_play = false;
        p.can_pause = false;
        assert_eq!(p.indicator(), Playback::Unavailable);
        assert_eq!(player("VLC", "Unknown").indicator(), Playback::Unavailable);
    }
    #[test]
    fn gnome_target_survives_pause_and_only_another_playing_player_can_promote() {
        let mut r = Router::default();
        let chrome = player("Chrome", "Playing");
        let vlc = player("VLC", "Paused");
        r.appeared(chrome.clone());
        r.appeared(vlc.clone());
        assert_eq!(r.status().player.as_deref(), Some("Chrome"));
        r.changed(player("Chrome", "Paused"));
        assert_eq!(r.status().player.as_deref(), Some("Chrome"));
        r.changed(player("VLC", "Playing"));
        assert_eq!(r.status().player.as_deref(), Some("VLC"));
        r.changed(chrome);
        assert_eq!(r.status().player.as_deref(), Some("VLC"));
        r.remove(&vlc.name);
        assert_eq!(r.status().player.as_deref(), Some("Chrome"));
        r.remove(&player("Chrome", "").name);
        assert_eq!(r.status(), Status::default());
    }
    #[test]
    fn new_paused_player_replaces_a_paused_target_and_native_calls_synchronize_selection() {
        let mut r = Router::default();
        let chrome = player("Chrome", "Paused");
        let vlc = player("VLC", "Paused");
        r.appeared(chrome.clone());
        r.appeared(vlc);
        assert_eq!(r.status().player.as_deref(), Some("VLC"));
        r.observed_target(&chrome.name);
        assert_eq!(r.status().player.as_deref(), Some("Chrome"));
        r.observed_target("missing");
        assert_eq!(r.status().player.as_deref(), Some("Chrome"));
        r.observed_target(":1.2");
        assert_eq!(r.status().player.as_deref(), Some("VLC"));
    }
}
