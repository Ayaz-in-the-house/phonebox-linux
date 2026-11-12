//! MPRIS D-Bus bağlantısı: olay akışı + denetim çağrıları. Tüm çağrılar `Connection::call_method` (vekil önbelleği yok,
//! her yenilemede yeni eşleşme kuralı eklenmez).

use super::{MPRIS_PREFIX, Media, PlayerProps};
use futures_util::StreamExt as _;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use zbus::{OwnedVariant as OwnedValue, Variant as Value};

const PATH: &str = "/org/mpris/MediaPlayer2";
const PLAYER: &str = "org.mpris.MediaPlayer2.Player";
const ROOT: &str = "org.mpris.MediaPlayer2";
const PROPS: &str = "org.freedesktop.DBus.Properties";
const DBUS: &str = "org.freedesktop.DBus";

fn set_source(m: &Media, s: impl Into<String>) {
    if let Ok(mut g) = m.source.lock() {
        *g = s.into();
    }
}

/// İzle; dönerse kaynak yok demektir (`media` = `null`, tek günlük satırı — sessiz hata yok).
pub async fn run(media: Arc<Media>) {
    if let Err(e) = follow(&media).await {
        eprintln!("phoned: MPRIS izlenemiyor ({e}); media = null");
        set_source(&media, format!("unavailable: {e}"));
    }
}

async fn follow(media: &Arc<Media>) -> zbus::Result<()> {
    let conn = zbus::Connection::session().await?;
    let _ = media.conn.set(conn.clone());
    let props_rule =
        zbus::MatchRule::builder().msg_type(zbus::message::Type::Signal).interface(PROPS).member("PropertiesChanged").path(PATH).build()?;
    let seek_rule = zbus::MatchRule::builder().msg_type(zbus::message::Type::Signal).interface(PLAYER).member("Seeked").path(PATH).build()?;
    let owner_rule = zbus::MatchRule::builder()
        .msg_type(zbus::message::Type::Signal)
        .sender(DBUS)
        .interface(DBUS)
        .member("NameOwnerChanged")
        .arg0ns("org.mpris.MediaPlayer2")
        .build()?;
    let a = zbus::MessageStream::for_match_rule(props_rule, &conn, std::num::NonZeroUsize::new(64)).await?;
    let b = zbus::MessageStream::for_match_rule(seek_rule, &conn, std::num::NonZeroUsize::new(64)).await?;
    let c = zbus::MessageStream::for_match_rule(owner_rule, &conn, std::num::NonZeroUsize::new(64)).await?;
    let mut events = futures_util::stream::select(futures_util::stream::select(a, b), c);

    // Benzersiz ad (":1.42") → MPRIS adı; sinyaller benzersiz adla gelir.
    let mut owners: HashMap<String, String> = HashMap::new();
    let names: Vec<String> = conn.call_method(Some(DBUS), "/org/freedesktop/DBus", Some(DBUS), "ListNames", &()).await?.body().deserialize()?;
    for n in names.into_iter().filter(|n| n.starts_with(MPRIS_PREFIX)) {
        refresh(&conn, media, &mut owners, &n).await;
    }
    set_source(media, "mpris");

    while let Some(msg) = events.next().await {
        let Ok(msg) = msg else { continue };
        media.events.fetch_add(1, Ordering::Relaxed);
        let h = msg.header();
        let member = h.member().map(|m| m.as_str().to_owned()).unwrap_or_default();
        if member == "NameOwnerChanged" {
            let Ok((name, _old, new)) = msg.body().deserialize::<(String, String, String)>() else { continue };
            if !name.starts_with(MPRIS_PREFIX) {
                continue;
            }
            owners.retain(|_, v| *v != name);
            if new.is_empty() {
                media.remove(&name);
            } else {
                refresh(&conn, media, &mut owners, &name).await;
            }
            continue;
        }
        let Some(sender) = h.sender().map(|s| s.as_str().to_owned()) else { continue };
        if let Some(name) = owners.get(&sender).cloned() {
            refresh(&conn, media, &mut owners, &name).await;
        }
    }
    Ok(())
}

async fn refresh(conn: &zbus::Connection, media: &Media, owners: &mut HashMap<String, String>, name: &str) {
    if let Ok(reply) = conn.call_method(Some(DBUS), "/org/freedesktop/DBus", Some(DBUS), "GetNameOwner", &(name,)).await
        && let Ok(owner) = reply.body().deserialize::<String>()
    {
        owners.insert(owner, name.to_owned());
    }
    match conn.call_method(Some(name), PATH, Some(PROPS), "GetAll", &(PLAYER,)).await {
        Ok(reply) => match reply.body().deserialize::<HashMap<String, OwnedValue>>() {
            Ok(map) => {
                let mut p = props_from(&map);
                // Kök arayüz (Identity, DesktopEntry): oyuncu adı için; yoksa veri yolu adına düşülür.
                if let Ok(root) = conn.call_method(Some(name), PATH, Some(PROPS), "GetAll", &(ROOT,)).await
                    && let Ok(rm) = root.body().deserialize::<HashMap<String, OwnedValue>>()
                {
                    p.identity = rm.get("Identity").and_then(|v| str_of(v)).unwrap_or_default();
                    p.desktop_entry = rm.get("DesktopEntry").and_then(|v| str_of(v)).unwrap_or_default();
                }
                media.put(name, p);
            }
            Err(e) => eprintln!("phoned: MPRIS {name}: özellikler çözülemedi ({e})"),
        },
        // Oyuncu kapanırken gelen son sinyal: sessizce kaldır (NameOwnerChanged da gelecek).
        Err(_) => media.remove(name),
    }
}

fn str_of(v: &Value<'_>) -> Option<String> {
    match v {
        Value::Str(s) => Some(s.as_str().to_owned()),
        Value::ObjectPath(p) => Some(p.as_str().to_owned()),
        Value::Variant(inner) => str_of(inner),
        _ => None,
    }
}

fn int_of(v: &Value<'_>) -> Option<i64> {
    match v {
        Value::I64(n) => Some(*n),
        Value::U64(n) => i64::try_from(*n).ok(),
        Value::I32(n) => Some(i64::from(*n)),
        Value::U32(n) => Some(i64::from(*n)),
        Value::F64(f) => Some(*f as i64),
        Value::Variant(inner) => int_of(inner),
        _ => None,
    }
}

fn strs_of(v: &Value<'_>) -> Vec<String> {
    match v {
        Value::Array(a) => a.iter().filter_map(str_of).collect(),
        Value::Variant(inner) => strs_of(inner),
        other => str_of(other).into_iter().collect(),
    }
}

/// `GetAll(Player)` → düz özellikler.
#[must_use]
pub fn props_from(map: &HashMap<String, OwnedValue>) -> PlayerProps {
    let get = |k: &str| map.get(k).map(|v| &**v);
    let mut p = PlayerProps {
        status: get("PlaybackStatus").and_then(str_of).unwrap_or_default(),
        position_us: get("Position").and_then(int_of).unwrap_or(0),
        can_seek: matches!(get("CanSeek"), Some(Value::Bool(true))),
        ..PlayerProps::default()
    };
    let md: Option<HashMap<String, OwnedValue>> = map.get("Metadata").and_then(|v| v.try_clone().ok()).and_then(|v| HashMap::try_from(v).ok());
    if let Some(md) = md {
        let mget = |k: &str| md.get(k).map(|v| &**v);
        p.title = mget("xesam:title").and_then(str_of).unwrap_or_default();
        p.artists = mget("xesam:artist").map(strs_of).unwrap_or_default();
        p.album = mget("xesam:album").and_then(str_of).unwrap_or_default();
        p.length_us = mget("mpris:length").and_then(int_of).unwrap_or(0);
        p.art_url = mget("mpris:artUrl").and_then(str_of).unwrap_or_default();
        p.track_id = mget("mpris:trackid").and_then(str_of).unwrap_or_default();
        p.url = mget("xesam:url").and_then(str_of).unwrap_or_default();
    }
    p
}

/// Panel eylemi (`playpause` · `next` · `prev` · `seek` + `ms` · `skip` + `±ms`). Bağlantı / oyuncu yoksa `false` (→ 400).
pub async fn control(media: &Media, action: &str, ms: Option<i64>) -> bool {
    let (Some(conn), Some((bus, props))) = (media.conn.get(), media.current()) else {
        return false;
    };
    let r = match (action, ms) {
        ("playpause", _) => conn.call_method(Some(bus.as_str()), PATH, Some(PLAYER), "PlayPause", &()).await,
        ("next", _) => conn.call_method(Some(bus.as_str()), PATH, Some(PLAYER), "Next", &()).await,
        ("prev", _) => conn.call_method(Some(bus.as_str()), PATH, Some(PLAYER), "Previous", &()).await,
        // K8: ±N ms göreli (MPRIS Seek, µs).
        ("skip", Some(ms)) => conn.call_method(Some(bus.as_str()), PATH, Some(PLAYER), "Seek", &(ms.saturating_mul(1000),)).await,
        ("seek", Some(ms)) => {
            let Ok(track) = zbus::ObjectPath::try_from(props.track_id.as_str()) else { return false };
            conn.call_method(Some(bus.as_str()), PATH, Some(PLAYER), "SetPosition", &(track, ms.max(0).saturating_mul(1000))).await
        }
        _ => return false,
    };
    match r {
        Ok(_) => true,
        Err(e) => {
            eprintln!("phoned: MPRIS {bus} {action}: {e}");
            false
        }
    }
}
