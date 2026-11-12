//! Pulse yerel soket istemcisi: tek görev, tek yazıcı. Bağlan → AUTH → ad → abone ol (sink + sunucu) → `@DEFAULT_SINK@` sorgula;
//! sonra yalnız olay ya da `/api/volume` isteğiyle uyanır (yoklama yok). Koparsa `audio = null`, `RETRY_MS` sonra yeniden dener.
//!
//! Okuma ayrı bir görevde paket paket kanala akar (yarım okunmuş paket `select!` iptaliyle kaybolmasın).

use super::{Audio, AudioSnapshot, CALL_MS, Cmd, Op, RETRY_MS, plan, proto};
use proto::{Error as PErr, Writer, cmd};
use std::sync::Arc;
use std::sync::atomic::Ordering::Relaxed;
use std::time::Duration;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::UnixStream;
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};
use tokio::sync::mpsc;

const DEFAULT_SINK: &str = "@DEFAULT_SINK@";

/// Sonsuz döngü: oturum aç, düşünce temizle, bekle, yeniden dene. `Audio` başına bir kez çalışır.
pub async fn run(audio: Arc<Audio>) {
    let Some(mut rx) = audio.take_rx() else { return };
    let mut last = String::new();
    loop {
        audio.connects.fetch_add(1, Relaxed);
        let r = session(&audio, &mut rx).await;
        audio.connected.store(false, Relaxed);
        audio.set(None);
        let why = match r {
            Ok(()) => return, // istek kanalı kapandı: ajan kapanıyor
            Err(e) => e,
        };
        if why != last {
            eprintln!("phoned: ses: {} → {why} ({} sn sonra yeniden)", audio.settings.socket.display(), RETRY_MS / 1000);
            last.clone_from(&why);
        }
        if let Ok(mut g) = audio.error.lock() {
            g.clone_from(&why);
        }
        // Beklerken gelen istekler hemen "yok" alır (kuyrukta 6 sn bekletilmez).
        let until = tokio::time::Instant::now() + Duration::from_millis(RETRY_MS);
        while let Ok(Some((_, reply))) = tokio::time::timeout_at(until, rx.recv()).await {
            let _ = reply.send(None);
        }
    }
}

async fn read_loop(mut rd: OwnedReadHalf, tx: mpsc::Sender<Vec<u8>>) {
    let mut h = [0u8; proto::HEADER];
    loop {
        if rd.read_exact(&mut h).await.is_err() {
            return;
        }
        let Ok((len, ch)) = proto::parse_header(&h) else { return };
        let mut body = vec![0u8; usize::try_from(len).unwrap_or(0)];
        if rd.read_exact(&mut body).await.is_err() {
            return;
        }
        if ch == proto::CONTROL && tx.send(body).await.is_err() {
            return;
        }
    }
}

/// Okuma görevi oturumla birlikte ölür.
struct Abort(tokio::task::JoinHandle<()>);
impl Drop for Abort {
    fn drop(&mut self) {
        self.0.abort();
    }
}

struct Conn<'a> {
    audio: &'a Audio,
    wr: OwnedWriteHalf,
    pkts: mpsc::Receiver<Vec<u8>>,
    tag: u32,
    /// İlgili bir olay geldi → bir sonraki fırsatta tek yeniden sorgu (art arda olaylar birleşir).
    dirty: bool,
}

async fn session(audio: &Audio, rx: &mut mpsc::Receiver<Cmd>) -> Result<(), String> {
    let stream = UnixStream::connect(&audio.settings.socket).await.map_err(|e| e.to_string())?;
    let cookie = read_cookie(audio).await;
    let (rd, wr) = stream.into_split();
    let (tx, pkts) = mpsc::channel(32);
    let _reader = Abort(tokio::spawn(read_loop(rd, tx)));
    let mut c = Conn { audio, wr, pkts, tag: 0, dirty: false };
    c.call(|t| proto::auth(t, &cookie)).await?;
    c.call(|t| proto::set_client_name(t, "phoned")).await?;
    c.call(|t| proto::subscribe(t, proto::MASK_SINK | proto::MASK_SERVER)).await?;
    let mut sink = c.query().await?;
    audio.connected.store(true, Relaxed);
    if let Ok(mut g) = audio.error.lock() {
        g.clear();
    }
    loop {
        if c.dirty {
            sink = c.query().await?;
        }
        tokio::select! {
            p = c.pkts.recv() => {
                let p = p.ok_or_else(|| "bağlantı kapandı".to_owned())?;
                c.on_unsolicited(&p);
            }
            m = rx.recv() => {
                let Some((op, reply)) = m else { return Ok(()) };
                let r = c.apply(op, &sink).await;
                let r = match r { Ok(s) => s, Err(e) => { let _ = reply.send(None); return Err(e); } };
                sink = r;
                let _ = reply.send(Some(AudioSnapshot::of(&sink)));
            }
        }
    }
}

async fn read_cookie(audio: &Audio) -> [u8; proto::COOKIE] {
    let mut c = [0u8; proto::COOKIE];
    if let Some(p) = &audio.settings.cookie
        && let Ok(b) = tokio::fs::read(p).await
        && let Some(src) = b.get(..proto::COOKIE)
    {
        c.copy_from_slice(src);
    }
    c
}

impl Conn<'_> {
    /// İstek gönder, aynı etiketli yanıtı bekle (arada gelen olaylar işlenir). Yanıt gövdesi (komut + etiket dahil) döner.
    async fn call(&mut self, build: impl FnOnce(u32) -> Writer) -> Result<Vec<u8>, String> {
        self.tag = self.tag.wrapping_add(1) % proto::INVALID;
        let tag = self.tag;
        self.wr.write_all(&build(tag).packet()).await.map_err(|e| e.to_string())?;
        let deadline = tokio::time::Instant::now() + Duration::from_millis(CALL_MS);
        loop {
            let p = tokio::time::timeout_at(deadline, self.pkts.recv())
                .await
                .map_err(|_| format!("yanıt yok ({CALL_MS} ms)"))?
                .ok_or_else(|| "bağlantı kapandı".to_owned())?;
            let (c, t, mut r) = proto::split(&p).map_err(|e| e.to_string())?;
            match c {
                cmd::REPLY if t == tag => return Ok(p),
                cmd::ERROR if t == tag => return Err(PErr::Server(r.u32().unwrap_or(0)).to_string()),
                _ => self.on_unsolicited(&p),
            }
        }
    }

    /// İstenmemiş paket: abonelik olayıysa ve ana düzeyi etkileyebiliyorsa yeniden sorgu işaretle; gerisi yok sayılır.
    fn on_unsolicited(&mut self, p: &[u8]) {
        let Ok((cmd::SUBSCRIBE_EVENT, _, mut r)) = proto::split(p) else { return };
        if let Ok((kind, _)) = proto::parse_event(&mut r)
            && proto::event_relevant(kind)
        {
            self.audio.events.fetch_add(1, Relaxed);
            self.dirty = true;
        }
    }

    /// Varsayılan sink'i sorgula, anlık görüntüyü yaz (değiştiyse akış uyanır).
    async fn query(&mut self) -> Result<proto::Sink, String> {
        self.dirty = false;
        let p = self.call(|t| proto::get_sink_info(t, DEFAULT_SINK)).await?;
        let (_, _, mut r) = proto::split(&p).map_err(|e| e.to_string())?;
        let s = proto::parse_sink(&mut r).map_err(|e| e.to_string())?;
        self.audio.queries.fetch_add(1, Relaxed);
        self.audio.set(Some(AudioSnapshot::of(&s)));
        Ok(s)
    }

    async fn apply(&mut self, op: Op, sink: &proto::Sink) -> Result<proto::Sink, String> {
        let p = plan(op, sink);
        let idx = sink.index;
        self.audio.sets.fetch_add(1, Relaxed);
        if let Some(v) = p.volumes {
            self.call(|t| proto::set_sink_volume(t, idx, &v)).await?;
        }
        if let Some(m) = p.mute {
            self.call(|t| proto::set_sink_mute(t, idx, m)).await?;
        }
        self.query().await
    }
}
