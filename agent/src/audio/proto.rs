//! PulseAudio yerel protokolünün (PipeWire `pipewire-pulse` da konuşur) phoned'un gerektirdiği alt kümesi — saf, G/Ç yok.
//!
//! Paket = 20 baytlık tanımlayıcı (`uzunluk, kanal, ofset_hi, ofset_lo, bayraklar`, hepsi u32 big-endian; denetim paketi kanal
//! `0xFFFF_FFFF`) + `tagstruct` gövdesi. Tagstruct: her değer bir etiket baytı + big-endian yük (`'L'` u32, `'t'` NUL bitimli
//! dizge, `'N'` boş dizge, `'x'` keyfi bayt, `'1'/'0'` bool, `'a'` örnek biçimi, `'m'` kanal haritası, `'v'` kanal düzeyleri,
//! `'P'` özellik listesi). Kaynak: pulseaudio `src/pulsecore/{tagstruct,pstream,native-common}.h`; laptopta salt-okuma sondayla
//! ölçüldü (fikstür `tests/fixtures/pulse/`).

/// Tanımlayıcı uzunluğu.
pub const HEADER: usize = 20;
/// Denetim paketi kanalı.
pub const CONTROL: u32 = 0xFFFF_FFFF;
/// Geçersiz dizin (`PA_INVALID_INDEX`): ada göre sorgu.
pub const INVALID: u32 = 0xFFFF_FFFF;
/// %100 düzey (`PA_VOLUME_NORM`).
pub const NORM: u32 = 0x1_0000;
/// İstemcinin bildirdiği protokol sürümü (13+: özellik listesiyle ad, 15+: sink yanıtında taban düzey vb.; biz yalnız öneki okuruz).
pub const VERSION: u32 = 32;
/// Tek paket üst sınırı (sunucu bozuk uzunluk yollarsa bellek şişmesin; gerçek sink yanıtı ~3,5 KB).
pub const MAX_PACKET: u32 = 1 << 20;
/// Çerez uzunluğu.
pub const COOKIE: usize = 256;

/// Komutlar (`native-common.h`).
pub mod cmd {
    pub const ERROR: u32 = 0;
    pub const REPLY: u32 = 2;
    pub const AUTH: u32 = 8;
    pub const SET_CLIENT_NAME: u32 = 9;
    pub const GET_SINK_INFO: u32 = 21;
    pub const SUBSCRIBE: u32 = 35;
    pub const SET_SINK_VOLUME: u32 = 36;
    pub const SET_SINK_MUTE: u32 = 39;
    pub const SUBSCRIBE_EVENT: u32 = 66;
}

/// Abonelik maskesi: sink + sunucu (varsayılan sink değişimi → `@DEFAULT_SINK@` yeniden sorgusu yeni sink'i verir).
pub const MASK_SINK: u32 = 0x0001;
pub const MASK_SERVER: u32 = 0x0080;
/// Olay türü: alt 4 bit tesis, 0x30 değişim türü.
pub const FACILITY_MASK: u32 = 0x0F;
pub const FACILITY_SINK: u32 = 0;
pub const FACILITY_SERVER: u32 = 7;

/// Protokol hatası (bozuk/kesik paket, beklenmeyen etiket, sunucu `ERROR`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    Short,
    Tag { want: u8, got: u8 },
    Utf8,
    TooBig(u32),
    Server(u32),
    Unexpected(u32),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Short => f.write_str("kesik paket"),
            Self::Tag { want, got } => write!(f, "etiket {:?} beklendi, {:?} geldi", char::from(*want), char::from(*got)),
            Self::Utf8 => f.write_str("dizge UTF-8 değil"),
            Self::TooBig(n) => write!(f, "paket çok büyük ({n} bayt)"),
            Self::Server(c) => write!(f, "sunucu hatası {c}"),
            Self::Unexpected(c) => write!(f, "beklenmeyen komut {c}"),
        }
    }
}

impl std::error::Error for Error {}

/// Tagstruct yazıcı.
#[derive(Debug, Default, Clone)]
pub struct Writer(pub Vec<u8>);

impl Writer {
    /// Komut + etiket başlıklı gövde.
    #[must_use]
    pub fn command(command: u32, tag: u32) -> Self {
        Self::default().u32(command).u32(tag)
    }
    #[must_use]
    pub fn u32(mut self, v: u32) -> Self {
        self.0.push(b'L');
        self.0.extend_from_slice(&v.to_be_bytes());
        self
    }
    #[must_use]
    pub fn string(mut self, v: Option<&str>) -> Self {
        match v {
            Some(s) => {
                self.0.push(b't');
                self.0.extend_from_slice(s.as_bytes());
                self.0.push(0);
            }
            None => self.0.push(b'N'),
        }
        self
    }
    #[must_use]
    pub fn arbitrary(mut self, b: &[u8]) -> Self {
        self.0.push(b'x');
        self.0.extend_from_slice(&u32::try_from(b.len()).unwrap_or(u32::MAX).to_be_bytes());
        self.0.extend_from_slice(b);
        self
    }
    #[must_use]
    pub fn boolean(mut self, v: bool) -> Self {
        self.0.push(if v { b'1' } else { b'0' });
        self
    }
    /// `'a'`: biçim, kanal sayısı, örnekleme hızı.
    #[must_use]
    pub fn sample_spec(mut self, format: u8, channels: u8, rate: u32) -> Self {
        self.0.extend_from_slice(&[b'a', format, channels]);
        self.0.extend_from_slice(&rate.to_be_bytes());
        self
    }
    #[must_use]
    pub fn channel_map(mut self, pos: &[u8]) -> Self {
        self.0.push(b'm');
        self.0.push(u8::try_from(pos.len()).unwrap_or(u8::MAX));
        self.0.extend_from_slice(pos);
        self
    }
    #[must_use]
    pub fn cvolume(mut self, v: &[u32]) -> Self {
        self.0.push(b'v');
        self.0.push(u8::try_from(v.len()).unwrap_or(u8::MAX));
        for x in v {
            self.0.extend_from_slice(&x.to_be_bytes());
        }
        self
    }
    /// `'P'`: (anahtar, değer) çiftleri; değer NUL bitimli dizge olarak (libpulse `pa_proplist_sets` eşi).
    #[must_use]
    pub fn proplist(mut self, kv: &[(&str, &str)]) -> Self {
        self.0.push(b'P');
        for (k, v) in kv {
            let mut val = v.as_bytes().to_vec();
            val.push(0);
            self = self.string(Some(k)).u32(u32::try_from(val.len()).unwrap_or(u32::MAX)).arbitrary(&val);
        }
        self.0.push(b'N');
        self
    }
    /// Denetim kanalında tam paket (tanımlayıcı + gövde).
    #[must_use]
    pub fn packet(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(HEADER + self.0.len());
        out.extend_from_slice(&u32::try_from(self.0.len()).unwrap_or(u32::MAX).to_be_bytes());
        out.extend_from_slice(&CONTROL.to_be_bytes());
        out.extend_from_slice(&[0; 12]);
        out.extend_from_slice(&self.0);
        out
    }
}

/// Tanımlayıcıdan gövde uzunluğu ve kanal. Bellek kapısı: `MAX_PACKET` üstü → hata.
///
/// # Errors
/// Uzunluk sınırı aşılırsa.
pub fn parse_header(h: &[u8; HEADER]) -> Result<(u32, u32), Error> {
    let len = u32::from_be_bytes([h[0], h[1], h[2], h[3]]);
    let ch = u32::from_be_bytes([h[4], h[5], h[6], h[7]]);
    if len > MAX_PACKET { Err(Error::TooBig(len)) } else { Ok((len, ch)) }
}

/// Tagstruct okuyucu.
#[derive(Debug, Clone)]
pub struct Reader<'a> {
    b: &'a [u8],
}

impl<'a> Reader<'a> {
    #[must_use]
    pub const fn new(b: &'a [u8]) -> Self {
        Self { b }
    }
    fn take(&mut self, n: usize) -> Result<&'a [u8], Error> {
        if self.b.len() < n {
            return Err(Error::Short);
        }
        let (h, t) = self.b.split_at(n);
        self.b = t;
        Ok(h)
    }
    fn tag(&mut self, want: u8) -> Result<(), Error> {
        let got = *self.take(1)?.first().ok_or(Error::Short)?;
        if got == want { Ok(()) } else { Err(Error::Tag { want, got }) }
    }
    fn be32(&mut self) -> Result<u32, Error> {
        let s = self.take(4)?;
        Ok(u32::from_be_bytes(s.try_into().map_err(|_| Error::Short)?))
    }
    /// # Errors
    /// Etiket/uzunluk uymazsa.
    pub fn u32(&mut self) -> Result<u32, Error> {
        self.tag(b'L')?;
        self.be32()
    }
    /// `'t'` → `Some`, `'N'` → `None`.
    ///
    /// # Errors
    /// Etiket/uzunluk uymazsa ya da UTF-8 değilse.
    pub fn string(&mut self) -> Result<Option<String>, Error> {
        match self.b.first() {
            Some(b'N') => {
                self.take(1)?;
                Ok(None)
            }
            _ => {
                self.tag(b't')?;
                let end = self.b.iter().position(|c| *c == 0).ok_or(Error::Short)?;
                let s = self.take(end)?;
                self.take(1)?;
                std::str::from_utf8(s).map(|s| Some(s.to_owned())).map_err(|_| Error::Utf8)
            }
        }
    }
    /// # Errors
    /// Etiket uymazsa.
    pub fn boolean(&mut self) -> Result<bool, Error> {
        match *self.take(1)?.first().ok_or(Error::Short)? {
            b'1' => Ok(true),
            b'0' => Ok(false),
            got => Err(Error::Tag { want: b'1', got }),
        }
    }
    /// Örnek biçimini atlar.
    ///
    /// # Errors
    /// Etiket/uzunluk uymazsa.
    pub fn skip_sample_spec(&mut self) -> Result<(), Error> {
        self.tag(b'a')?;
        self.take(6).map(|_| ())
    }
    /// # Errors
    /// Etiket/uzunluk uymazsa.
    pub fn channel_map(&mut self) -> Result<Vec<u8>, Error> {
        self.tag(b'm')?;
        let n = usize::from(*self.take(1)?.first().ok_or(Error::Short)?);
        Ok(self.take(n)?.to_vec())
    }
    /// # Errors
    /// Etiket/uzunluk uymazsa.
    pub fn cvolume(&mut self) -> Result<Vec<u32>, Error> {
        self.tag(b'v')?;
        let n = usize::from(*self.take(1)?.first().ok_or(Error::Short)?);
        (0..n).map(|_| self.be32()).collect()
    }
}

/// Bir denetim paketinin başı: komut + etiket + geri kalan okuyucu.
///
/// # Errors
/// Kesik paket.
pub fn split(body: &[u8]) -> Result<(u32, u32, Reader<'_>), Error> {
    let mut r = Reader::new(body);
    let c = r.u32()?;
    let t = r.u32()?;
    Ok((c, t, r))
}

/// Ana düzey sink'in phoned'un kullandığı alanları.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sink {
    pub index: u32,
    pub name: String,
    pub volumes: Vec<u32>,
    pub mute: bool,
}

/// `GET_SINK_INFO` yanıt gövdesi (komut + etiket sonrası). Yalnız önek okunur: dizin, ad, açıklama, örnek biçimi, kanal haritası,
/// sahip modül, kanal düzeyleri, sessiz. Sonrası (sürüme göre değişen alanlar) okunmaz.
///
/// # Errors
/// Kesik ya da etiketi uymayan yanıt.
pub fn parse_sink(r: &mut Reader<'_>) -> Result<Sink, Error> {
    let index = r.u32()?;
    let name = r.string()?.unwrap_or_default();
    let _desc = r.string()?;
    r.skip_sample_spec()?;
    let _map = r.channel_map()?;
    let _owner = r.u32()?;
    let volumes = r.cvolume()?;
    let mute = r.boolean()?;
    Ok(Sink { index, name, volumes, mute })
}

/// `SUBSCRIBE_EVENT` gövdesi (komut + etiket sonrası): (olay türü, dizin).
///
/// # Errors
/// Kesik ya da etiketi uymayan paket.
pub fn parse_event(r: &mut Reader<'_>) -> Result<(u32, u32), Error> {
    Ok((r.u32()?, r.u32()?))
}

/// Olay ana düzeyi etkileyebilir mi (sink ya da sunucu tesisi; ötekiler — örneğin kaynak, akış — yeniden sorgu doğurmaz).
#[must_use]
pub const fn event_relevant(kind: u32) -> bool {
    matches!(kind & FACILITY_MASK, FACILITY_SINK | FACILITY_SERVER)
}

/// Kanal düzeylerinden 0.6.9 yüzdesi: en yüksek kanal (`pa_cvolume_max`, `pactl`/`wpctl` gösterimi) / NORM × 100, yuvarlak.
#[must_use]
pub fn percent(volumes: &[u32]) -> i64 {
    let max = volumes.iter().copied().max().unwrap_or(0);
    (i64::from(max) * 100 + i64::from(NORM) / 2) / i64::from(NORM)
}

/// Yüzde → pa düzeyi (0..=100 kıskaçlı).
#[must_use]
pub fn from_percent(level: i64) -> u32 {
    let l = level.clamp(0, 100);
    u32::try_from((l * i64::from(NORM) + 50) / 100).unwrap_or(NORM)
}

// --- istek gövdeleri ---

#[must_use]
pub fn auth(tag: u32, cookie: &[u8; COOKIE]) -> Writer {
    Writer::command(cmd::AUTH, tag).u32(VERSION).arbitrary(cookie)
}

#[must_use]
pub fn set_client_name(tag: u32, name: &str) -> Writer {
    Writer::command(cmd::SET_CLIENT_NAME, tag).proplist(&[("application.name", name)])
}

/// Ada göre (`@DEFAULT_SINK@` de geçerli) sink sorgusu.
#[must_use]
pub fn get_sink_info(tag: u32, name: &str) -> Writer {
    Writer::command(cmd::GET_SINK_INFO, tag).u32(INVALID).string(Some(name))
}

#[must_use]
pub fn subscribe(tag: u32, mask: u32) -> Writer {
    Writer::command(cmd::SUBSCRIBE, tag).u32(mask)
}

#[must_use]
pub fn set_sink_volume(tag: u32, index: u32, volumes: &[u32]) -> Writer {
    Writer::command(cmd::SET_SINK_VOLUME, tag).u32(index).string(None).cvolume(volumes)
}

#[must_use]
pub fn set_sink_mute(tag: u32, index: u32, mute: bool) -> Writer {
    Writer::command(cmd::SET_SINK_MUTE, tag).u32(index).string(None).boolean(mute)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fix(name: &str) -> Vec<u8> {
        let p = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/pulse/");
        let s = std::fs::read_to_string(format!("{p}{name}.hex")).unwrap();
        (0..s.trim().len()).step_by(2).map(|i| u8::from_str_radix(&s.trim()[i..i + 2], 16).unwrap()).collect()
    }

    fn body(pkt: &[u8]) -> &[u8] {
        &pkt[HEADER..]
    }

    #[test]
    fn tp_w24b_a1_requests_match_real_probe_bytes() {
        assert_eq!(auth(1, &[0; COOKIE]).packet(), fix("auth.req"));
        assert_eq!(get_sink_info(4, "@DEFAULT_SINK@").packet(), fix("get_sink_info.req"));
        assert_eq!(subscribe(5, MASK_SINK | MASK_SERVER).packet(), fix("subscribe.req"));
        let h: [u8; HEADER] = fix("auth.req")[..HEADER].try_into().unwrap();
        assert_eq!(parse_header(&h), Ok((276, CONTROL)));
        let mut big = h;
        big[..4].copy_from_slice(&(MAX_PACKET + 1).to_be_bytes());
        assert_eq!(parse_header(&big), Err(Error::TooBig(MAX_PACKET + 1)));
    }

    #[test]
    fn tp_w24b_a1_auth_reply_version() {
        let p = fix("auth.reply");
        let (c, t, mut r) = split(body(&p)).unwrap();
        assert_eq!((c, t, r.u32().unwrap()), (cmd::REPLY, 1, 35));
    }

    #[test]
    fn tp_w24b_a2_real_sink_reply() {
        let p = fix("sink_info.reply");
        let (c, t, mut r) = split(body(&p)).unwrap();
        assert_eq!((c, t), (cmd::REPLY, 4));
        let s = parse_sink(&mut r).unwrap();
        assert_eq!(s.index, 201_009);
        assert!(s.name.starts_with("alsa_output."), "{}", s.name);
        assert_eq!(s.volumes, vec![0, 0]);
        assert!(!s.mute);
        assert_eq!(percent(&s.volumes), 0);
    }

    #[test]
    fn tp_w24b_a2_synthetic_sink_max_channel_and_mute() {
        let w = Writer::command(cmd::REPLY, 9)
            .u32(7)
            .string(Some("s"))
            .string(None)
            .sample_spec(3, 2, 48_000)
            .channel_map(&[1, 2])
            .u32(INVALID)
            .cvolume(&[32_768, 65_536])
            .boolean(true);
        let (_, _, mut r) = split(&w.0).unwrap();
        let s = parse_sink(&mut r).unwrap();
        assert_eq!((s.index, s.mute, percent(&s.volumes)), (7, true, 100));
        assert_eq!(percent(&[19_661]), 30);
        // Kesik yanıt: hata, panik yok.
        for cut in 0..w.0.len() {
            let (c, _, mut r) = match split(&w.0[..cut]) {
                Ok(x) => x,
                Err(e) => {
                    assert_eq!(e, Error::Short);
                    continue;
                }
            };
            assert_eq!(c, cmd::REPLY);
            assert!(parse_sink(&mut r).is_err(), "{cut}");
        }
    }

    #[test]
    fn tp_w24b_a4_event_parse_and_relevance() {
        let w = Writer::command(cmd::SUBSCRIBE_EVENT, INVALID).u32(0x10 | FACILITY_SINK).u32(201_009);
        let (c, _, mut r) = split(&w.0).unwrap();
        assert_eq!(c, cmd::SUBSCRIBE_EVENT);
        let (kind, idx) = parse_event(&mut r).unwrap();
        assert_eq!((kind, idx), (0x10, 201_009));
        assert!(event_relevant(kind));
        assert!(event_relevant(0x10 | FACILITY_SERVER));
        assert!(!event_relevant(0x10 | 1), "kaynak (mikrofon) olayı yeniden sorgu doğurmaz");
        assert!(!event_relevant(0x20 | 2), "sink-input olayı yeniden sorgu doğurmaz");
    }

    #[test]
    fn tp_w24b_a5_percent_roundtrip() {
        assert_eq!(from_percent(40), 26_214);
        assert_eq!(from_percent(100), NORM);
        assert_eq!(from_percent(150), NORM);
        assert_eq!(from_percent(-3), 0);
        for l in 0..=100 {
            assert_eq!(percent(&[from_percent(l)]), l);
        }
    }

    #[test]
    fn set_requests_shape() {
        let w = set_sink_volume(3, 7, &[1, 2]);
        let (c, t, mut r) = split(&w.0).unwrap();
        assert_eq!((c, t, r.u32().unwrap(), r.string().unwrap(), r.cvolume().unwrap()), (cmd::SET_SINK_VOLUME, 3, 7, None, vec![1, 2]));
        let w = set_sink_mute(4, 7, true);
        let (c, _, mut r) = split(&w.0).unwrap();
        assert_eq!((c, r.u32().unwrap(), r.string().unwrap(), r.boolean().unwrap()), (cmd::SET_SINK_MUTE, 7, None, true));
        let w = set_client_name(2, "phoned");
        let (c, _, r) = split(&w.0).unwrap();
        assert_eq!(c, cmd::SET_CLIENT_NAME);
        assert_eq!(r.b.first(), Some(&b'P'));
    }
}
