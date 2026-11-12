//! phoned — PhoneBox 0.6.9 telefon panelinin Fedora ajanı (Rust). Ayrıntı: README.md, docs/DECISIONS.md.
//!
//! Katmanlar (bağımlılık yönü yukarıdan aşağı): `app` (HTTP) → `stream` (tempo) · `hub` (konu + örnekleyici) ·
//! `config` (ayar deposu) · `guard` (kapı) · `session` (logind) · `actions` (W2.3 panel eylemleri → `herdr` · `zen` · `adb` soket istemcileri) · `procs` · `kill` · `media` · `audio` · `debug` (W2.4) · `dictate` (K11: telefon dikte → herdr-web → yapıştırma) · `compat` (W2.5: ayar sayfasının kapsam dışı okumaları) → `collect` (işletim sistemi okuma, `Collector` trait).

pub mod actions;
pub mod adb;
pub mod app;
pub mod audio;
pub mod collect;
pub mod compat;
pub mod config;
pub mod debug;
pub mod dictate;
pub mod gpu;
pub mod guard;
pub mod herdr;
pub mod hub;
pub mod kill;
pub mod media;
pub mod procs;
pub mod session;
pub mod stream;
pub mod zen;

pub use app::{Args, USAGE, serve};
