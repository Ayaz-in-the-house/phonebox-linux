//! Oturum kilidi: logind `LockedHint` (zbus, sistem veri yolu), olay güdümlü — yoklama yok.
//!
//! Kullanıcı servisi bir oturumun içinde koşmaz; bu yüzden önce kullanıcının grafik oturumu (`user/self` → `Display`),
//! o yoksa `session/auto` okunur. Kaynak yoksa ya da koparsa `locked = false` (bugünkü davranış: 1 Hz) ve tek günlük satırı —
//! deneyim kısılmaz, sessiz hata yok (TP-W2-S3).

use futures_util::StreamExt as _;
use tokio::sync::watch;
use zbus::OwnedObjectPath;

const LOGIN1: &str = "org.freedesktop.login1";

/// İzle ve `tx`'e yaz; dönerse kilit bilgisi yok demektir (`false` yazılmış olur).
pub async fn watch_locked(tx: watch::Sender<bool>) {
    match follow(&tx).await {
        Ok(()) => eprintln!("phoned: logind kilit akışı bitti; kilit yok sayılıyor"),
        Err(e) => eprintln!("phoned: oturum kilidi izlenemiyor ({e}); kilit yok sayılıyor (1 Hz)"),
    }
    tx.send_replace(false);
}

async fn session_path(conn: &zbus::Connection) -> zbus::Result<OwnedObjectPath> {
    let user = zbus::Proxy::new(conn, LOGIN1, "/org/freedesktop/login1/user/self", "org.freedesktop.login1.User").await?;
    if let Ok((id, path)) = user.get_property::<(String, OwnedObjectPath)>("Display").await
        && !id.is_empty()
        && path.as_str() != "/"
    {
        return Ok(path);
    }
    let mgr = zbus::Proxy::new(conn, LOGIN1, "/org/freedesktop/login1", "org.freedesktop.login1.Manager").await?;
    mgr.call("GetSession", &("auto",)).await
}

async fn follow(tx: &watch::Sender<bool>) -> zbus::Result<()> {
    let conn = zbus::Connection::system().await?;
    let path = session_path(&conn).await?;
    let session = zbus::Proxy::new(&conn, LOGIN1, path, "org.freedesktop.login1.Session").await?;
    let mut changes = session.receive_property_changed::<bool>("LockedHint").await;
    tx.send_replace(session.get_property::<bool>("LockedHint").await?);
    while let Some(change) = changes.next().await {
        if let Ok(locked) = change.get().await {
            tx.send_replace(locked);
        }
    }
    Ok(())
}
