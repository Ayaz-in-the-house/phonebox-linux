"""TP-W3-01..07, 11 — W3 panel yamaları (W3-PANEL.md). Yamasız (upstream) RED, PATCH_DIR=build/www GREEN beklenir.
  flock "$XDG_RUNTIME_DIR/phone-panel-pw.lock" timeout 300 systemd-run --user --scope -p MemoryMax=1G -p MemorySwapMax=0 --quiet \
    -E PATCH_DIR=build/www python3 -P tests/test_w3_panel_pw.py <çıktı-dizini> [tp01 tp02 …]
Çıkış 0 = seçilen TP'lerin hepsi PASS. Sahte ajan + sahte köken: canlı ajana/telefona dokunmaz (test_w3_harness.py).
"""
import copy, os, sys
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from playwright.sync_api import sync_playwright
import test_w3_harness as H

OUT = sys.argv[1] if len(sys.argv) > 1 else "."
SEL = set(sys.argv[2:])


def tp01(b):
    """Görünür sayfa dışı güncelleme 0."""
    ctx, page, _ = H.open_panel(b)
    H.goto(page, 0)
    page.evaluate("(s) => window.__startFeed(s, true)", H.STATE)
    page.wait_for_timeout(3000)
    page.evaluate("window.__w3reset()")
    page.wait_for_timeout(10000)
    c = H.counters(page)
    ctx.close()
    hidden = {k: (c["mut"].get(k, 0), c["draw"].get(k, 0)) for k in ("1", "2", "3")}
    vis = (c["mut"].get("0", 0), c["draw"].get("0", 0))
    ok = all(m == 0 and d == 0 for m, d in hidden.values()) and vis[0] > 0 and vis[1] > 0  # görünür sayfa güncellenmeli (negatif kontrol)
    return H.verdict("tp01", ok, {"hidden_mut_draw": hidden, "visible_mut_draw": vis}, OUT)


CMP = """() => import('/panel/main.js').then(m => {
  const c = document.querySelector('#pager .page:nth-child(1) .w-chart canvas');
  const a = c.getContext('2d').getImageData(0, 0, c.width, c.height).data;
  const r = document.createElement('canvas');
  r.style.cssText = `position:fixed;left:0;top:0;width:${c.clientWidth}px;height:${c.clientHeight}px`;
  document.body.appendChild(r);
  const s = m.D.state;
  m.chart(r, ['cpu', 'gpu'].map(k => ({ data: m.hist[k], color: m.color(m.METRICS[k].color), max: m.METRICS[k].max(s) })), { grid: true });
  const b = r.getContext('2d').getImageData(0, 0, r.width, r.height).data;
  r.remove();
  if (a.length !== b.length) return { same_size: false };
  let bad = 0, nonblank = 0;
  for (let i = 0; i < a.length; i += 4) {
    if (a[i + 3]) nonblank++;
    // görünen renk = ön-çarpımlı (rgb·a); neredeyse saydam pikselde ön-çarpımsız rgb anlamsız gürültüdür
    const pa = (x, j) => x[j] * x[i + 3] / 255;
    if (Math.max(Math.abs(a[i + 3] - b[i + 3]), Math.abs(pa(a, i) - pa(b, i)), Math.abs(pa(a, i + 1) - pa(b, i + 1)), Math.abs(pa(a, i + 2) - pa(b, i + 2))) > 8) bad++;
  }
  return { same_size: true, px: a.length / 4, bad, nonblank, id: c.__w3 || 0, samples: m.hist.cpu.length };
})"""


def tp02(b):
    """Artımlı grafik: seri başına lineTo/örnek ≤ 12, tam çizimle piksel eşit."""
    ctx, page, _ = H.open_panel(b)
    H.goto(page, 0)
    # geçmiş dolu (90 örnek) — telefonda 90 sn sonraki kalıcı durum; tam çizim seri başına ≈ 91 lineTo
    page.evaluate("import('/panel/main.js').then(m => { for (const k in m.hist) { m.hist[k].length = 0; for (let i = 0; i < m.HIST; i++) m.hist[k].push(i % 50); } })")
    page.evaluate("(s) => window.__startFeed(s, true)", H.STATE)
    page.wait_for_timeout(4000)
    page.evaluate("window.__w3reset()")
    page.wait_for_timeout(10000)
    samples = 10
    c = H.counters(page)
    cmp = page.evaluate(CMP)
    ctx.close()
    per = c["lineTo"].get(str(cmp.get("id", 0)), 0) / samples / 2  # 2 seri
    ok = cmp.get("same_size") and cmp["nonblank"] > 0 and cmp["bad"] <= cmp["px"] * 0.005 and per <= 12
    return H.verdict("tp02", ok, {"lineTo_per_series_per_sample": round(per, 1), "samples": samples, "pixel": cmp}, OUT)


ANIM = """() => document.getAnimations().map(a => { const t = a.effect.getComputedTiming();
  return { name: a.animationName || a.transitionProperty || '?', state: a.playState, iter: t.iterations === Infinity ? 'inf' : t.iterations,
           el: (a.effect.target && a.effect.target.className && String(a.effect.target.className.baseVal ?? a.effect.target.className)) || '' }; })"""


def tp03(b):
    """Kalıcı animasyon 0, sonlu animasyonlar biter."""
    ctx, page, _ = H.open_panel(b)
    H.goto(page, 2)
    page.evaluate("(s) => window.__startFeed(s, false)", H.STATE)
    page.wait_for_timeout(1500)
    early = page.evaluate(ANIM)
    page.wait_for_timeout(8000)
    an = page.evaluate(ANIM)
    shot = os.path.join(OUT, "tp03-home.png")
    page.screenshot(path=shot)
    ctx.close()
    inf = [a for a in an if a["iter"] == "inf"]
    running = [a for a in an if a["state"] == "running"]
    ok = not inf and not running
    return H.verdict("tp03", ok, {"early": early[:12], "after_8s": an[:12], "infinite": len(inf), "running": len(running)}, OUT)


def tp04(b):
    """Yerel örnek saati: 2,5 sn olay kesintisinde 10 sn'de 10 ± 1 örnek."""
    ctx, page, _ = H.open_panel(b)
    H.goto(page, 0)
    page.evaluate("(s) => window.__startFeed(s, true)", H.STATE)
    page.wait_for_timeout(2500)
    n0 = page.evaluate("import('/panel/main.js').then(m => m.hist.cpu.length)")
    page.wait_for_timeout(3000)
    page.evaluate("window.__pauseUntil = performance.now() + 2500")
    page.wait_for_timeout(7000)
    n1 = page.evaluate("import('/panel/main.js').then(m => m.hist.cpu.length)")
    ctx.close()
    got = n1 - n0
    return H.verdict("tp04", 9 <= got <= 11, {"samples_in_10s": got, "start_len": n0}, OUT)


def tp05(b):
    """Aynı durum ikinci kez → görünür sayfada DOM değişikliği 0."""
    ctx, page, _ = H.open_panel(b)
    H.goto(page, 0)
    page.evaluate("window.onAgentStream('open')")
    s = copy.deepcopy(H.STATE)
    s["ts"] = 1
    page.evaluate("(s) => window.__feed(s)", s)
    page.wait_for_timeout(400)
    page.evaluate("(s) => window.__feed(s)", s)
    page.wait_for_timeout(400)
    page.evaluate("window.__w3reset()")
    page.evaluate("(s) => window.__feed(s)", s)
    page.wait_for_timeout(400)
    same = H.counters(page)["mut"].get("0", 0)
    s2 = copy.deepcopy(s)
    s2["pc"]["cpu"] = 77.0
    page.evaluate("window.__w3reset()")
    page.evaluate("(s) => window.__feed(s)", s2)
    page.wait_for_timeout(400)
    changed = H.counters(page)["mut"].get("0", 0)
    ctx.close()
    return H.verdict("tp05", same == 0 and changed > 0, {"mutations_same_state": same, "mutations_cpu_changed": changed}, OUT)


def tp06(b):
    """fast tiki: durmuş medya → 0; çalan medya (negatif kontrol) → ≥ 36 ve çubuk ilerler."""
    res = {}
    for name, media in (("idle", None), ("playing", H.PLAYING)):
        st = copy.deepcopy(H.STATE)
        st["media"] = media
        ctx, page, _ = H.open_panel(b, state=st)
        H.goto(page, 2)
        page.evaluate("(s) => window.__startFeed(s, false)", st)
        page.wait_for_timeout(2000)
        w0 = page.evaluate("document.querySelector('.w-media .fill').style.width")
        page.evaluate("window.__w3reset()")
        page.wait_for_timeout(10000)
        w1 = page.evaluate("document.querySelector('.w-media .fill').style.width")
        res[name] = {"fast_ticks": H.counters(page)["timers"].get("250", 0), "fill": [w0, w1]}
        ctx.close()
    p = res["playing"]
    ok = res["idle"]["fast_ticks"] == 0 and p["fast_ticks"] >= 36 and float(p["fill"][1].rstrip("%") or 0) > float(p["fill"][0].rstrip("%") or 0)
    return H.verdict("tp06", ok, res, OUT)


def tp07(b):
    """B5 panel içinde: değerler, grafik, swap, süreç kapatma arayüzü (istek gitmeden)."""
    ctx, page, _ = H.open_panel(b)
    H.goto(page, 0)
    page.evaluate("(s) => window.__startFeed(s, true)", H.STATE)
    page.wait_for_timeout(4000)
    vals = page.evaluate("""() => Object.fromEntries([...document.querySelectorAll('#pager .page:nth-child(1) .w')].map((w, i) =>
      [i + ':' + w.className.split(' ')[1], (w.querySelector('.t-val, .g-val, .s-main, .meters')?.textContent || '') + ' | ' + (w.querySelector('.t-sub, .g-sub')?.textContent || '')]))""")
    canv = page.evaluate("""() => [...document.querySelectorAll('#pager .page:nth-child(1) canvas')].map(c => {
      const d = c.getContext('2d').getImageData(0, 0, c.width, c.height).data; let n = 0; for (let i = 3; i < d.length; i += 4) if (d[i]) n++; return n; })""")
    page.screenshot(path=os.path.join(OUT, "tp07-system.png"))
    swap_sub = page.evaluate("document.querySelectorAll('#pager .page:nth-child(1) .w')[6].querySelector('.t-sub').textContent")
    swap_label = page.evaluate("document.querySelectorAll('#pager .page:nth-child(1) .w')[6].querySelector('.t-label').textContent")
    H.goto(page, 3)
    page.wait_for_selector(".w-processes .proc", timeout=5000)
    page.click(".w-processes .proc")
    page.wait_for_selector("#psList .ps-btn", timeout=5000)
    btn = page.evaluate("(() => { const b = document.querySelector('#psList .ps-btn'); return { text: b.textContent, disabled: b.disabled }; })()")
    # tıklama + okuma aynı JS turunda: sayfa listesi 2 sn'de bir yeniden kurulur (upstream loadSheet), yarış olmasın
    after = page.evaluate("(() => { const b = document.querySelector('#psList .ps-btn'); b.click(); return b.textContent; })()")
    page.wait_for_timeout(300)
    page.screenshot(path=os.path.join(OUT, "tp07-procs.png"))
    kills = [p for p in page.posts if "/procs/kill" in p]
    ctx.close()
    texts = list(vals.values())
    ok = (all("—" not in t.split("|")[0] for t in texts[:6]) and all(n > 0 for n in canv)
          and swap_label.strip().upper() == "SWAP" and "12.3 / 100 GB" in swap_sub
          and not btn["disabled"] and btn["text"] == "End" and after == "Confirm" and not kills)
    return H.verdict("tp07", ok, {"values": vals, "canvas_nonblank_px": canv, "swap": [swap_label, swap_sub],
                                  "end_btn": btn, "after_first_tap": after, "kill_posts": kills}, OUT)


def tp11(b):
    """Kaydırırken komşu taze: parmak değince sayfa 1 güncel durumu gösterir."""
    ctx, page, cdp = H.open_panel(b, rate=1)
    H.goto(page, 0)
    page.evaluate("window.onAgentStream('open')")
    s = copy.deepcopy(H.STATE)
    s["pc"]["cpu"] = 11.0
    page.evaluate("(s) => window.__feed(s)", s)
    page.wait_for_timeout(300)
    s["pc"]["cpu"] = 66.0
    page.evaluate("(s) => window.__feed(s)", s)
    page.wait_for_timeout(300)
    cdp.send("Input.dispatchTouchEvent", {"type": "touchStart", "touchPoints": [{"x": 700, "y": 200}]})
    cdp.send("Input.dispatchTouchEvent", {"type": "touchMove", "touchPoints": [{"x": 650, "y": 200}]})
    page.wait_for_timeout(100)
    v = page.evaluate("document.querySelector('#pager .page:nth-child(2) .w-stat .t-val b').textContent")
    cdp.send("Input.dispatchTouchEvent", {"type": "touchEnd", "touchPoints": []})
    ctx.close()
    return H.verdict("tp11", v == "66", {"neighbour_cpu_text": v}, OUT)


TPS = {"tp01": tp01, "tp02": tp02, "tp03": tp03, "tp04": tp04, "tp05": tp05, "tp06": tp06, "tp07": tp07, "tp11": tp11}

if __name__ == "__main__":
    os.makedirs(OUT, exist_ok=True)
    oks = []
    with sync_playwright() as p:
        b = p.chromium.launch(executable_path=H.exe())
        for k, f in TPS.items():
            if SEL and k not in SEL:
                continue
            try:
                oks.append(f(b))
            except Exception as e:  # bir TP'nin hatası öbürlerini durdurmasın; FAIL sayılır
                oks.append(H.verdict(k, False, {"error": f"{type(e).__name__}: {e}"[:400]}, OUT))
        b.close()
    print(f"PASS: {sum(oks)}/{len(oks)}")
    sys.exit(0 if all(oks) else 1)
