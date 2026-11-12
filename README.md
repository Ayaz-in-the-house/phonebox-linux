# phonebox-linux

An efficiency-focused Linux build of [PhoneBox](https://github.com/ersinkoc/phonebox) — the desk panel that turns an old Android
phone into a live system / media / control screen over USB.

This repository is **not a fork with history**. It is a measured write-up of what we changed on top of **PhoneBox 0.6.9**
(`620c904`), offered back to upstream:

* `agent/` — a drop-in **Rust desktop agent** that speaks the same HTTP + SSE contract as the 0.6.9 .NET agent (same APK, same panel).
* `patches/` — **13 small, independent panel patches** (one topic each, applied in order with `git apply`), each with its own test and mutant.
* `apk/` — a pinned, reproducible container build for the APK (digest-pinned base image, checksummed SDK / Gradle).
* `bench/` — the headless-Chromium harnesses that produced the panel numbers below.

Upstream has since moved to 0.9.x. We are too far behind to send clean PRs, so this repository documents *what* we changed,
*why*, and *what it bought*, so the ideas can be applied upstream in whatever form fits — after which we will follow upstream again.

---

## 1. Results first

All numbers are medians of repeated runs; the measurement setup is in [§5](#5-how-it-was-measured). "Upstream" = PhoneBox 0.6.9.

### 1.1 Desktop agent (the process that runs on the Linux PC)

| Metric | Upstream 0.6.9 agent (.NET 10) | `agent/` (Rust) | Change |
|---|---|---|---|
| Idle CPU, panel connected | **8.7 – 11.7 %** of one core (60 s and 120 s windows) | **0.05 %** live stream · **0.0 %** no client | ≈ −99.5 % |
| CPU, 1 h soak (client connected, GPU sampler on) | — | **0.118 %** average | — |
| Resident memory | 36 – 88 MB (cgroup 94 MB) | **3.0 – 5.3 MB** (1 h: 5.0 → 4.7 MB, no growth) | ≈ −92 % |
| Threads | 19 | 1 – 2 (single-threaded tokio) | — |
| External processes per second | 1.5 – 2.5 (`playerctl`, `wpctl`, `adb`) | **0** (D-Bus MPRIS, native PulseAudio protocol, `adb track-devices`) | eliminated |
| `/proc/*/fd` full scan | every 2 s, always | only while a client looks at the process list | on demand |
| SSE bytes while nobody is looking | 832 B/s (full ≈ 800 B snapshot every second, unchanged fields included) | **≈ 21 B/s** (state every 30 s + keep-alive comments) | ≈ −97 % |
| SSE while the panel is visible | 832 B/s, 1 Hz | 604 – 855 B/s, 1 Hz, max gap 1.006 s | same contract |
| Quiet → live wake-up | — | 999 ms to the first full state | ≤ 1 s |
| External volume change → panel event | polling | **63 ms** (event-driven) | — |
| `POST /api/volume` → event on the stream | — | **13 ms** | — |
| Contract parity | — | 20 GET endpoints + SSE compared field by field against 0.6.9: 0 red | drop-in |

### 1.2 Panel (the WebView on the phone) — layer by layer

Same phone UI, same config, fake 1 Hz agent, headless Chromium 873×393 touch, CPU ×4 throttle. RED = unpatched 0.6.9, GREEN = patched.

| Layer | What was wasted | Upstream 0.6.9 | Patched | Patch |
|---|---|---|---|---|
| Visibility | Off-screen neighbour page updated every second | 82 DOM mutations + 420 canvas calls / 10 s | **0 + 0** | 0002 |
| Timers | 250 ms "fast" tick always on | 40 ticks / 10 s with paused media | **0** (still 40 while media plays) | 0003 |
| Animation | Endless CSS loops (blink, pulse, glow) repaint every frame | 5 infinite animations | **0** | 0004 |
| Sampling | Chart only advanced when an event arrived | 8 samples / 10 s with a 2.5 s event gap | **10** | 0005 |
| Drawing | Whole chart path redrawn every second | 92.5 `lineTo` per series per sample | **8.5** (pixel diff vs full redraw 0.04 %) | 0006 |
| DOM | Unchanged values rewritten every second | 25 DOM mutations for an identical state | **0** | 0007 |
| Compositing | Settle animation repainted + re-rastered the page strip | 69.6 raster tasks / swipe (after 0001) | **25** (upstream 40.4) | 0009 |
| GPU thread | same | 219 ms / swipe (after 0001) | **134 ms** (upstream 139) | 0009 |
| Gesture | Page settle | 520 – 690 ms (native smooth scroll) | **≈ 223 ms**, 0 missed pages | 0001 |
| Input | Swipe starting on a slider changed the volume | 16 unintended volume changes in 9 min (phone log) | **0** (tap / vertical / hold-drag only, ≤ 8 req/s) | 0010 |

Every patch has a mutant check: reverting its key line turns its test red (`6/6` for 0002–0007, plus 0009 and 0010).

### 1.3 Transport (USB `adb reverse`)

| Metric | Value |
|---|---|
| Phone → agent request (`/api/info`) | median 5.6 ms (TCP connect 2.3 ms); local loopback 0.61 ms → the tunnel adds ≈ 5 ms |
| Bulk throughput | 28 – 35 MB/s |
| Live stream share of capacity | ≈ 0.003 % |

**Conclusion:** the cable is never the bottleneck. Cost lives at the two ends — the agent producing data nobody looks at, and the
panel repainting things that did not change. That is where all the changes above are.

### 1.4 Phone baseline (before) and what is still open

| Metric (unpatched 0.6.9, panel idle on screen) | Value |
|---|---|
| Panel app process | 22.9 – 24.2 % of one core, 181 MB |
| WebView renderer | 19.2 – 20.1 % of one core, 188 MB |
| gfxinfo | 36 % janky frames, p50 21 ms |
| Battery temperature (30 min, charging) | 22.9 → 26.3 °C |

On-device *after* numbers for the full series are not in this write-up yet; the desktop harness reproduces raster / GPU *work* counts
reliably, but its janky-frame percentage is not comparable to a phone (software GPU saturates at 76 – 82 % for every variant).

---

## 2. Where we changed what, how, and what it gained

Read this table right after the numbers above: every row is one problem we measured, where it lived, the technique we used, and
the measured gain. "Layer" follows the path of one value: collected on the PC → shipped over USB → drawn on the phone.

| Layer | Where (file / component) | Problem we measured | How we solved it | Gain |
|---|---|---|---|---|
| L1 · agent runtime | .NET agent process | 8.7 – 11.7 % of a core idle, 36 – 88 MB, 19 threads | Rewrote the agent in Rust (single-threaded tokio, no JIT/GC), same HTTP + SSE contract, contract tests against 0.6.9 shapes | CPU **0.05 %** live / **0.0 %** idle, RSS **3 – 5 MB** |
| L1 · collection | `SystemMonitor`, `PrivacyMonitor`, monitor loops | 15 loops run whether anyone looks or not; full `/proc/*/fd` scan every 2 s | **Subscription-gated monitors**: a collector runs only while a connected client needs its topic; process list is pull-based | nothing is read when nobody looks |
| L2 · process spawning | `playerctl`, `wpctl`, `adb devices`, `nvidia-smi` | 1.5 – 2.5 fork/exec per second | Native protocols instead of CLI tools: MPRIS over D-Bus (`zbus`), PulseAudio wire protocol over its Unix socket (works on PipeWire), persistent `adb track-devices` socket; `nvidia-smi` only while subscribed | **0** child processes for audio/media/adb (289 samples, 60 s) |
| L2 · audio events | volume / mute | polling, latency bound by the poll interval | Server-side PulseAudio subscription → push on change | external change → panel **63 ms**; `POST /api/volume` → stream **13 ms** |
| L3 · stream content | `/api/stream` (SSE) | full ≈ 800 B snapshot every second incl. constant fields | Two modes: 1 Hz while the panel is visible; **quiet mode** (locked / night / no viewer) = state every 30 s + keep-alive comments under the APK's 8 s watchdog | **832 → ≈ 21 B/s** when nobody looks; wake-up to live ≤ 1 s |
| L3 · transport | USB `adb reverse` | suspected bottleneck | Measured instead of changed: 5.6 ms per request, 28 – 35 MB/s | stream uses ≈ 0.003 % → no change needed |
| L4 · update scheduling | `main.js` `emit()` / `activeSet` | off-screen neighbour pages updated every second | Update only the visible page; neighbours refreshed once on touchstart | 82 DOM + 420 canvas ops / 10 s → **0** (0002) |
| L4 · timers | `main.js` 250 ms `fast` tick | 4 wake-ups per second forever | One-shot tick armed only when a visible widget wants it (`fastWanted`) | 40 → **0** ticks / 10 s when idle (0003) |
| L4 · sampling | `applyState` → chart history | chart advanced only on events; missed seconds compressed the time axis | Panel samples from its own 1 s clock; number and chart painted in one frame | 8 → **10** samples / 10 s with a 2.5 s gap (0005) |
| L5 · DOM writes | `stat`, `gauge`, `meters`, `media`, `clock` widgets | 82.9 % of DOM writes were no-ops | `setText` / `setKids` / `setVar`: write only on change | 25 → **0** mutations for an unchanged state (0007) |
| L6 · canvas | `pc.js` `chart()` | whole path redrawn each second | Incremental chart: shift bitmap left (`drawImage 'copy'`), redraw the last 3 steps, full redraw every 90 samples | 92.5 → **8.5** `lineTo` per sample, pixel diff 0.04 % (0006) |
| L6 · animation | `style.css` blink / pulse / glow | 5 infinite animations = repaint every frame | Clock colon driven by the existing 1 s tick; pulses limited to 3 – 4 iterations | 5 → **0** running animations (0004) |
| L7 · compositing | pager settle (`overflow-x: hidden`) | settle frames re-rastered the page strip → "Slow issue draw commands" on the phone | `.page { will-change: transform }` – each page its own compositor layer | raster 69.6 → **25** / swipe, GPU 219 → **134 ms** (0009) |
| L7 · gesture | pager `goPage()` | native smooth-scroll settle 520 – 690 ms | Own rAF settle (200 ms ease-out) with distance / velocity threshold | settle **≈ 223 ms**, 0 missed pages (0001) |
| Input | media / volume sliders | horizontal page swipe started on a slider changed the volume (16× in 9 min) | Gesture intent: horizontal = pager, value only on vertical / press-and-hold drag; ≤ 8 req/s, last value always sent | **0** unintended changes (0010) |
| Feature | media widget | Firefox hides position → "0:00 / 0:00" | `posKnown`, ±10 s, YouTube queue fields | correct display (0011) |
| Feature | Android shell + panel | no microphone path; WebView drops POST bodies | audio-capture-only permission for the app's own origin, body bridge, press-and-hold dictation → agent `POST /api/dictate` | phone works as a speech-to-text mic (0012, 0013) |
| Safety | process kill, lints | kill always enabled | kill off by default + fixed deny list + two-step confirm; clippy denies `unwrap`/`panic`/indexing; `cargo deny` | — |

## 3. Version map

| | Upstream | This repository |
|---|---|---|
| Base | PhoneBox **0.6.9** (`620c904`) | 0.6.9 + `patches/0001…0013` + Rust agent |
| Latest upstream checked | **0.9.4** (`419c6c0`) | — |
| Desktop agent | .NET 10, ~6 200 lines | Rust, single binary, same JSON contract |
| Panel | `assets/www` | same files + 13 patches |
| APK | upstream release | rebuilt in a pinned container, own signing key |

Items we found that are **still present in 0.9.4** (re-checked against the 0.9.4 source):

| # | Item | Where in 0.9.4 | Our answer |
|---|---|---|---|
| 1 | 250 ms fast tick always running | `main.js` (`setInterval` 250) | patch 0003 |
| 2 | `touch-action: pan-y` on lists swallows horizontal page swipes | `style.css` (3 places) | 0.9.3 fixed drag-*start* on lists, not the swallowed swipe |
| 3 | GitHub release check runs even with `updates.auto = false` | `UpdateService.cs` | no network update check (static `/api/update`) |
| 4 | `adb devices` spawned every 4 s | `AdbReverse.cs` | `adb track-devices` (one long-lived stream) |

What 0.9.x adds that we do not have: the 0.8.0 Control Deck widgets (mostly Windows-native), appearance presets (0.9.0),
webhook limits (0.9.2), Podman support and page wrap (0.9.3), and the 0.9.4 audit fixes (no lost events after an agent restart,
no duplicate POST over Wi-Fi, alarms on every page, RFC 5545 calendar). Our patch series applies cleanly to 0.6.9 (13/13);
against 0.9.4, 4 of the first 11 apply cleanly and 7 conflict in `main.js` / `style.css` / `basic.js`.

---

## 4. Patch series


Each patch header states *why*, *what*, the *measurement* and the *mutant*. Apply in order on a clean 0.6.9 tree:

```bash
git clone https://github.com/ersinkoc/phonebox && cd phonebox && git checkout 620c904
for p in /path/to/phonebox-linux/patches/[0-9]*.patch; do git apply "$p"; done
```

| # | Topic |
|---|---|
| 0001 | pager: fast settle + page buttons in landscape |
| 0002 | only the visible page is updated |
| 0003 | 250 ms tick only when needed |
| 0004 | no endless CSS animations |
| 0005 | chart samples from the panel's own 1 s clock |
| 0006 | incremental chart |
| 0007 | skip unchanged DOM leaves |
| 0008 | swap metric |
| 0009 | each page on its own compositor layer |
| 0010 | page swipe never drags the volume slider |
| 0011 | media: YouTube client, unknown position, ±10 s |
| 0012 | native shell: microphone permission (audio capture only, own origin only) + POST body bridge |
| 0013 | dictation button (press and hold) |

0001–0010 are pure efficiency / UX fixes and are the best upstream candidates. 0011–0013 depend on agent features.

---

**Agent tests:** contract tests against 0.6.9 response shapes (`agent/tests/fixtures/contract/`), HTTP / audio / adb / kill / dictate suites
(125+ tests), and mutant scripts (`agent/tests/*_mutants.sh`, `agent/tools/mutants.sh`) — 39/39 mutants killed at the last run.

## 5. How it was measured

| Layer | Tool | Notes |
|---|---|---|
| Agent CPU | `systemctl --user show -p CPUUsageNSec` deltas, `/proc/<pid>/stat` ticks | 60 s windows + 1 h soak; `top` sampling was too noisy and not used |
| Agent memory | `ps -o rss,nlwp`, cgroup `MemoryCurrent` | |
| Stream | `timeout 60 curl -sN …/api/stream \| wc -c`, event counts | |
| Transport | `adb shell curl -w '%{time_connect} %{time_total}'` ×10 | |
| Panel DOM / canvas / timers / animations | `bench/test_w3_panel_pw.py` (Playwright, fake agent via route interception) | deterministic, no phone needed |
| Raster / GPU | `bench/test_w3_scroll_pw.mjs` (Chromium tracing, real touch events, fresh browser per run, 3 reps, rotated order) | GPU raster forced on SwiftShader |
| Phone | `adb shell top`, `/proc` ticks, `dumpsys gfxinfo`, `dumpsys battery` | |

Known limits: desktop janky-% is not comparable to a phone; first-swipe GPU work is ≈ 2× (cold shader cache) so phones should be
warmed up 2–3 min before measuring; per-page layers cost GPU memory that was not measured on the device.

---

## 6. Build

**Agent** (Rust, toolchain pinned in `agent/rust-toolchain.toml`, dependencies pinned with `=` and `Cargo.lock`):

```bash
cd agent
cargo build --release --locked
cargo nextest run --locked          # or: cargo test --locked
./target/release/phoned --port 8765 --www <path to PhoneBox android/app/src/main/assets/www>
```

Flags: `--port`, `--config`, `--seed-from <0.6.9 settings file>` (copied once, never written back), `--www`, `--agent-dir`,
`--adb <path>`, `--packages <comma list>`. Environment: `HERDR_URL`, `HERDR_SOCKET_PATH` (optional terminal-agent integration),
`PHONED_DICTATE_URL`, `PHONED_DICTATE_TOKEN_FILE`, `PHONED_DICTATE_PASTE` (dictation backend), `PULSE_SERVER`, `PULSE_COOKIE`.
See [`PLACEHOLDERS.md`](PLACEHOLDERS.md) for values you must replace.

**APK**: `apk/Dockerfile` + `apk/in-container.sh` build the upstream source with the patch series inside a digest-pinned container;
versions and checksums are in `apk/versions.lock`. You need your own signing key (`in-container.sh keygen`); nothing is pushed to a
phone automatically.

## License

MIT (see `LICENSE`). PhoneBox is MIT-licensed by its author; the patches in `patches/` modify PhoneBox sources — see `NOTICE`.
