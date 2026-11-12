"""W3 ölçüm düzeneği (yardımcı, kendi başına koşmaz) — W3-PANEL.md § Test noktaları.
Panel dosyaları WWW dizininden (PATCH_DIR=build/www → yamalı; yoksa upstream 0.6.9) sahte bir köken altında sunulur
(http://panel.test/panel/…); /agent/api/* sahte ajandır (sabit yapılandırma, kayıtlı POST'lar). Canlı ajana, telefona, dış ağa gitmez.
Sayaçlar (sayfa yüklenmeden kurulur): sayfa bölümüne göre DOM değişikliği, canvas çizim çağrısı, zamanlayıcı geri çağrıları (gecikmeye göre).
Ağır ölçüm kapısı (COMMON.md): flock "$XDG_RUNTIME_DIR/phone-panel-pw.lock" timeout 300 systemd-run --user --scope -p MemoryMax=1G …
"""
import copy, glob, json, mimetypes, os

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)
UPSTREAM = os.path.expanduser("~/.cartography/refpool/ph-phonebox/android/app/src/main/assets/www")
ORIGIN = "http://panel.test"
URL = ORIGIN + "/panel/index.html"


def www():
    return os.path.abspath(os.environ.get("PATCH_DIR") or UPSTREAM)


def exe():
    shells = sorted(glob.glob(os.path.expanduser("~/.cache/ms-playwright/chromium_headless_shell-*/*/chrome-headless-shell")))
    return os.environ.get("PW_CHROME") or (shells[-1] if shells else None)


def W(id, type, x, y, w, h, **o):
    return {"id": id, "type": type, "x": x, "y": y, "w": w, "h": h, "options": o}


# Sayfa 0 = sistem (görünür), 1 = komşu sistem sayfası (cur ± 1 içinde, ama görünmez), 2 = ev (saat, medya, oturum, ON AIR), 3 = süreçler.
CFG = {
    "weather": {"city": "X", "lat": 0, "lon": 0},
    "night": {"enabled": False, "start": "00:30", "end": "07:30", "brightness": 0.02},
    "display": {"showSeconds": True, "idleReturnSec": 0, "vibrate": False, "volumeKeys": True},
    "shortcuts": [],
    "pages": [
        {"id": "sys", "title": "System", "enabled": True, "widgets": [
            W("cpu", "stat", 0, 0, 3, 2, metric="cpu"), W("ram", "stat", 3, 0, 3, 2, metric="ram"),
            W("gpu", "gauge", 6, 0, 3, 2, metric="gpu"), W("net", "stat", 9, 0, 3, 2, metric="net"),
            W("ch", "chart", 0, 2, 6, 2, metrics=["cpu", "gpu"]), W("mt", "meters", 6, 2, 6, 2),
            W("swap", "stat", 0, 4, 3, 2, metric="swap"), W("vr", "gauge", 3, 4, 3, 2, metric="ram"),
            W("nch", "chart", 6, 4, 6, 2, metrics=["down", "up"])]},
        {"id": "sys2", "title": "System 2", "enabled": True, "widgets": [
            W("cpu2", "stat", 0, 0, 6, 3, metric="cpu"), W("ch2", "chart", 6, 0, 6, 3, metrics=["ram"]),
            W("g2", "gauge", 0, 3, 6, 3, metric="cpu")]},
        {"id": "home", "title": "Home", "enabled": True, "widgets": [
            W("clock", "clock", 0, 0, 6, 3), W("media", "media", 6, 0, 6, 3),
            W("sess", "sessions", 0, 3, 6, 3), W("air", "onair", 6, 3, 6, 3)]},
        {"id": "procs", "title": "Processes", "enabled": True, "widgets": [W("pr", "processes", 0, 0, 12, 6, allowKill=True)]},
    ],
}

STATE = {
    "ts": 0, "cfgVersion": 1, "evSeq": 0, "events": [], "rules": [], "pushVersion": 1,
    "pc": {"cpu": 23.0, "cpuName": "Example 8-Core CPU", "cpuTemp": 61.0, "ramUsedGb": 21.4, "ramTotalGb": 62.0,
           "swapUsedGb": 12.3, "swapTotalGb": 100.0, "netDownBps": 32000, "netUpBps": 9000, "uptimeSec": 93000,
           "disks": [{"name": "/", "usedGb": 512, "totalGb": 930}],
           "gpu": {"util": 7, "vramUsed": 1.2, "vramTotal": 8, "temp": 48, "power": 12, "fan": 30, "clock": 800, "name": "RTX 4060"}},
    "ping": {"ms": 11, "avgMs": 12, "lossPct": 0},
    "media": None, "audio": {"volume": 35, "muted": False},
    "privacy": {"mic": True, "camera": False, "micApps": ["test"]},
}
PLAYING = {"app": "Test", "title": "Song", "artist": "A", "album": "B", "durMs": 200000, "posMs": 10000, "playing": True,
           "canSeek": True, "artId": 0}
WORK = {"sessions": [{"name": "w3", "tool": "claude", "status": "busy", "since": 0},
                     {"name": "w4", "tool": "codex", "status": "waiting", "since": 0},
                     {"name": "w5", "tool": "claude", "status": "shell", "since": 0}]}
PROCS = {"total": 3, "groupCount": 2, "groups": [
    {"name": "zen", "title": "Zen", "count": 2, "cpu": 12.5, "memMb": 900, "locked": False},
    {"name": "make", "title": "make", "count": 1, "cpu": 3.0, "memMb": 120, "locked": False}]}
DETAIL = {"name": "zen", "title": "Zen", "locked": False,
          "instances": [{"pid": 4242, "cpu": 6.2, "memMb": 450, "window": "tab", "args": "", "locked": False, "startedAt": 0}]}

INIT = r"""(() => {
  const C = window.__w3 = { mut: {}, draw: {}, lineTo: {}, timers: {}, canvases: [] };
  const sec = (n) => { const p = document.getElementById('pager'); if (!p) return -1;
    const s = n && (n.nodeType === 1 ? n : n.parentElement)?.closest?.('section.page'); return s ? [...p.children].indexOf(s) : -1; };
  window.__w3sec = sec;
  const P = CanvasRenderingContext2D.prototype;
  for (const k of ['lineTo', 'moveTo', 'fill', 'stroke', 'clearRect', 'drawImage', 'fillRect']) {
    const f = P[k];
    P[k] = function (...a) { const c = this.canvas; if (!c.__w3) { c.__w3 = C.canvases.push(c); }
      const s = sec(c); C.draw[s] = (C.draw[s] || 0) + 1; if (k === 'lineTo') C.lineTo[c.__w3] = (C.lineTo[c.__w3] || 0) + 1;
      return f.apply(this, a); };
  }
  for (const k of ['setInterval', 'setTimeout']) {
    const f = window[k];
    window[k] = function (cb, ms, ...a) { const d = Math.round(+ms || 0);
      const w = typeof cb === 'function' ? function (...x) { C.timers[d] = (C.timers[d] || 0) + 1; return cb.apply(this, x); } : cb;
      return f.call(window, w, ms, ...a); };
  }
  const start = () => { const p = document.getElementById('pager');
    new MutationObserver((rs) => { for (const r of rs) { const s = sec(r.target); C.mut[s] = (C.mut[s] || 0) + 1; } })
      .observe(p, { subtree: true, childList: true, attributes: true, characterData: true }); };
  document.readyState === 'loading' ? document.addEventListener('DOMContentLoaded', start) : start();
  window.__w3reset = () => { C.mut = {}; C.draw = {}; C.lineTo = {}; C.timers = {}; };
  window.__feed = (s) => window.onAgentStream('state', JSON.stringify(s));
  window.__startFeed = (base, vary) => { let i = 0; window.__pauseUntil = 0; window.onAgentStream('open');
    clearInterval(window.__fi);
    window.__fi = setInterval(() => { if (performance.now() < window.__pauseUntil) return; i++;
      const s = JSON.parse(JSON.stringify(base)); s.ts = Date.now();
      if (vary) { s.pc.cpu = 10 + (i * 7) % 80; s.pc.gpu.util = 5 + (i * 11) % 90; s.pc.netDownBps = 1000 * (1 + (i * 13) % 50);
        s.pc.ramUsedGb = 20 + (i % 7) / 10; }
      window.__feed(s); }, 1000); };
  window.__stopFeed = () => clearInterval(window.__fi);
})()"""


def open_panel(b, cfg=None, size=(873, 393), rate=4, work=None, procs=None, state=None):
    cfg = cfg or CFG
    root = www()
    posts = []
    ctx = b.new_context(viewport={"width": size[0], "height": size[1]}, has_touch=True, is_mobile=True, device_scale_factor=2.75)
    st = copy.deepcopy(state or STATE)
    api = {"/config": {"config": cfg, "version": 1}, "/state": st, "/work": work or WORK, "/procs": procs or PROCS,
           "/procs/detail": DETAIL, "/quotas": {"providers": []}, "/push": {"items": {}, "version": 1},
           "/calendar": {"events": []}, "/approvals": {"items": []}}

    def handle(route):
        r = route.request
        url = r.url.split("?")[0]
        if not url.startswith(ORIGIN):
            return route.abort()
        path = url[len(ORIGIN):]
        if path.startswith("/agent/api"):
            p = path[len("/agent/api"):]
            if r.method != "GET":
                posts.append(r.method + " " + p)
                return route.fulfill(status=200, body="{}", content_type="application/json")
            body = api.get(p)
            if body is None:
                return route.fulfill(status=404, body="")
            return route.fulfill(status=200, body=json.dumps(body), content_type="application/json")
        if path.startswith("/panel/"):
            f = os.path.normpath(os.path.join(root, path[len("/panel/"):]))
            if not f.startswith(root) or not os.path.isfile(f):
                return route.fulfill(status=404, body="")
            ct = mimetypes.guess_type(f)[0] or "application/octet-stream"
            if f.endswith(".js"):
                ct = "text/javascript"
            return route.fulfill(status=200, body=open(f, "rb").read(), content_type=ct)
        return route.fulfill(status=404, body="")

    ctx.route("**/*", handle)
    ctx.add_init_script(INIT)
    page = ctx.new_page()
    cdp = ctx.new_cdp_session(page)
    page.goto(URL)
    page.wait_for_function("document.querySelectorAll('#pager .page').length >= 1")
    page.wait_for_timeout(1500)
    if rate and rate > 1:
        cdp.send("Emulation.setCPUThrottlingRate", {"rate": rate})
    page.posts = posts
    return ctx, page, cdp


def goto(page, i):
    page.evaluate("(i) => import('/panel/main.js').then(m => m.goPage(i, true))", i)
    page.wait_for_timeout(300)


def counters(page):
    return page.evaluate("JSON.parse(JSON.stringify(window.__w3, (k, v) => k === 'canvases' ? undefined : v))")


def verdict(name, ok, res, out):
    res["verdict"] = "PASS" if ok else "FAIL"
    res["www"] = www()
    os.makedirs(out, exist_ok=True)
    json.dump(res, open(os.path.join(out, f"{name}.json"), "w"), indent=1)
    print(name, res["verdict"], json.dumps(res, ensure_ascii=False)[:1500])
    return ok
