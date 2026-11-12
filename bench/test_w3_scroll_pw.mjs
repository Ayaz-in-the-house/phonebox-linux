// TP-W3-12..14 — parmakla sayfa kaydırma sırasında iş dağılımı (PERF-SCROLL-2026-11-10.md). Ağır ölçüm: filoda (buildbox) koşar.
//   PW_CORE=<playwright driver/package> PW_CHROME=<chrome> node tests/test_w3_scroll_pw.mjs --out <dizin> \
//     --www ad=dizin[,ad=dizin…] [--cfg live-config.json] [--rate 6] [--reps 3] [--rounds 3] [--gpu swiftshader|none] [--timeline]
//     [--gate <ad>]   TP-W3-12: <ad> (www listesinde 'upstream' de olmalı) kaydırma başına raster / oturma raster'ı / GPU iş parçacığı
//                     ≤ upstream × 1,1, oturma ≤ 300 ms, her kaydırma sayfaya oturur → çıkış 0, değilse 1
// Sahte köken (http://panel.test), sahte ajan (/agent/api/*), 1 Hz değişen durum; canlı ajana / telefona / dış ağa gitmez.
// Her ölçüm TAZE tarayıcı (+ boş kullanıcı dizini): gölgelendirici önbelleği soğuk başlar (telefonda `adb install -r` karşılığı).
// Kaydırma: System sayfası (canvas grafikleri) çevresinde gerçek dokunma olayları (Input.dispatchTouchEvent) — 4 kaydırma × rounds.
// İz (Chromium tracing) kaydırma penceresinde ölçülür; iş parçacığı başına meşguliyet, Paint/Raster sayıları, kare durumu.
import { createRequire } from 'module';
import fs from 'fs';
import path from 'path';
const require = createRequire(import.meta.url);
const { chromium } = require(process.env.PW_CORE || 'playwright-core');

const arg = (k, d) => { const i = process.argv.indexOf('--' + k); return i > 0 ? process.argv[i + 1] : d; };
const OUT = arg('out', '.'), RATE = +arg('rate', 6), REPS = +arg('reps', 3), ROUNDS = +arg('rounds', 3), GPU = arg('gpu', 'swiftshader');
const TIMELINE = process.argv.includes('--timeline');
const VARIANTS = arg('www', '').split(',').filter(Boolean).map((s) => s.split('='));
const ORIGIN = 'http://panel.test', URL0 = ORIGIN + '/panel/index.html';
const W = (id, type, x, y, w, h, o = {}) => ({ id, type, x, y, w, h, options: o });
const DEFAULT_CFG = {
  weather: { city: 'X', lat: 0, lon: 0 }, night: { enabled: false }, display: { showSeconds: true, idleReturnSec: 0, vibrate: false },
  shortcuts: [], pages: [
    { id: 'home', title: 'Home', enabled: true, widgets: [W('c', 'clock', 0, 0, 6, 3), W('s1', 'stat', 6, 0, 3, 3, { metric: 'cpu' }), W('s2', 'stat', 9, 0, 3, 3, { metric: 'ram' })] },
    { id: 'sys', title: 'System', enabled: true, widgets: [W('ch', 'chart', 0, 0, 6, 3, { metrics: ['cpu', 'gpu'] }), W('ch2', 'chart', 6, 0, 6, 3, { metrics: ['down', 'up'] }), W('mt', 'meters', 0, 3, 12, 3)] },
    { id: 'procs', title: 'Processes', enabled: true, widgets: [W('pr', 'processes', 0, 0, 12, 6)] }] };
const STATE = { ts: 0, cfgVersion: 1, evSeq: 0, events: [], rules: [], pushVersion: 1,
  pc: { cpu: 23, cpuName: 'CPU', cpuTemp: 61, ramUsedGb: 21.4, ramTotalGb: 62, swapUsedGb: 12.3, swapTotalGb: 100, netDownBps: 32000, netUpBps: 9000,
    uptimeSec: 93000, disks: [{ name: '/', usedGb: 512, totalGb: 930 }], gpu: { util: 7, vramUsed: 1.2, vramTotal: 8, temp: 48, power: 12, fan: 30, clock: 800, name: 'GPU' } },
  ping: { ms: 11, avgMs: 12, lossPct: 0 }, media: null, audio: { volume: 35, muted: false }, privacy: { mic: false, camera: false, micApps: [] } };
const WORK = { sessions: [{ name: 'w3', tool: 'claude', status: 'busy', since: 0 }] };
const PROCS = { total: 1, groupCount: 1, groups: [{ name: 'zen', title: 'Zen', count: 1, cpu: 12.5, memMb: 900, locked: false }] };
const CFG = arg('cfg') ? JSON.parse(fs.readFileSync(arg('cfg'))).config : DEFAULT_CFG;
CFG.night = { ...(CFG.night || {}), enabled: false };
CFG.display = { ...(CFG.display || {}), idleReturnSec: 0 };
const SYS = Math.max(0, CFG.pages.filter((p) => p.enabled !== false).findIndex((p) => p.widgets.some((w) => w.type === 'chart')));

const INIT = `(() => {
  window.__feed = (s) => window.onAgentStream && window.onAgentStream('state', JSON.stringify(s));
  window.__startFeed = (base) => { let i = 0; window.onAgentStream('open'); clearInterval(window.__fi);
    window.__fi = setInterval(() => { i++; const s = JSON.parse(JSON.stringify(base)); s.ts = Date.now();
      s.pc.cpu = 10 + (i * 7) % 80; s.pc.gpu.util = 5 + (i * 11) % 90; s.pc.netDownBps = 1000 * (1 + (i * 13) % 50); s.pc.ramUsedGb = 20 + (i % 7) / 10;
      window.__feed(s); }, 1000); };
})()`;
const MIME = { '.js': 'text/javascript', '.css': 'text/css', '.html': 'text/html', '.svg': 'image/svg+xml', '.png': 'image/png', '.json': 'application/json' };

async function open(browser, root) {
  const ctx = await browser.newContext({ viewport: { width: 873, height: 393 }, hasTouch: true, isMobile: true, deviceScaleFactor: 2.75 });
  const api = { '/config': { config: CFG, version: 1 }, '/state': STATE, '/work': WORK, '/procs': PROCS, '/quotas': { providers: [] },
    '/push': { items: {}, version: 1 }, '/calendar': { events: [] }, '/approvals': { items: [] } };
  await ctx.route('**/*', (route) => {
    const r = route.request(), url = r.url().split('?')[0];
    if (!url.startsWith(ORIGIN)) return route.abort();
    const p = url.slice(ORIGIN.length);
    if (p.startsWith('/agent/api')) {
      if (r.method() !== 'GET') return route.fulfill({ status: 200, body: '{}', contentType: 'application/json' });
      const b = api[p.slice('/agent/api'.length)];
      return b ? route.fulfill({ status: 200, body: JSON.stringify(b), contentType: 'application/json' }) : route.fulfill({ status: 404, body: '' });
    }
    if (p.startsWith('/panel/')) {
      const f = path.normalize(path.join(root, p.slice(7)));
      if (!f.startsWith(root) || !fs.existsSync(f) || !fs.statSync(f).isFile()) return route.fulfill({ status: 404, body: '' });
      return route.fulfill({ status: 200, body: fs.readFileSync(f), contentType: MIME[path.extname(f)] || 'application/octet-stream' });
    }
    return route.fulfill({ status: 404, body: '' });
  });
  await ctx.addInitScript(INIT);
  const page = await ctx.newPage();
  const cdp = await ctx.newCDPSession(page);
  await page.goto(URL0);
  await page.waitForFunction("document.querySelectorAll('#pager .page').length >= 1");
  await page.evaluate((s) => window.__startFeed(s), STATE);
  await page.evaluate((i) => import('/panel/main.js').then((m) => m.goPage(i, true)), SYS);
  await page.waitForTimeout(2500);   // grafik geçmişi birikir, ilk çizimler biter
  await cdp.send('Emulation.setCPUThrottlingRate', { rate: RATE });
  return { ctx, page, cdp };
}

async function swipe(page, cdp, dir) {
  const w = 873, y = 200, x0 = dir > 0 ? w * 0.8 : w * 0.2, x1 = dir > 0 ? w * 0.2 : w * 0.8, N = 12;
  await page.evaluate(() => { const pg = document.getElementById('pager'); window.__fr = [];
    if (!window.__loop) { window.__loop = 1; const loop = (t) => { if (window.__fr.length < 4000) window.__fr.push([t, pg.scrollLeft]); requestAnimationFrame(loop); }; requestAnimationFrame(loop); } });
  await cdp.send('Input.dispatchTouchEvent', { type: 'touchStart', touchPoints: [{ x: x0, y }] });
  for (let i = 1; i <= N; i++) {
    await cdp.send('Input.dispatchTouchEvent', { type: 'touchMove', touchPoints: [{ x: x0 + (x1 - x0) * i / N, y }] });
    await page.waitForTimeout(16);
  }
  const tUp = await page.evaluate('performance.now()');
  await cdp.send('Input.dispatchTouchEvent', { type: 'touchEnd', touchPoints: [] });
  await page.waitForTimeout(900);
  return page.evaluate((tUp) => { const fr = window.__fr, w = innerWidth, end = fr[fr.length - 1][1];
    let st = null; for (let i = 0; i < fr.length; i++) if (fr[i][0] >= tUp && fr.slice(i).every((f) => Math.abs(f[1] - end) < 1)) { st = fr[i][0]; break; }
    // geri tepme: oturma sırasında yön değişimi (tarayıcı fling'i ile JS oturması çekişirse ≥ 1)
    let rev = 0, sg = 0; for (let i = 1; i < fr.length; i++) { if (fr[i][0] < tUp) continue; const d = fr[i][1] - fr[i - 1][1];
      if (Math.abs(d) < 0.5) continue; const s = Math.sign(d); if (sg && s !== sg) rev++; sg = s; }
    const trace = fr.filter((f) => f[0] >= tUp - 60 && f[0] <= tUp + 400).map((f) => [Math.round(f[0] - tUp), Math.round(f[1])]);
    return { trace, settle_ms: st == null ? null : Math.round(st - tUp), on_page: Math.abs(end / w - Math.round(end / w)) < 0.01, reversals: rev }; }, tUp);
}

// ---- iz çözümleme
const TOP = new Set(['ThreadControllerImpl::RunTask', 'ThreadPool_RunTask', 'TaskGraphRunner::RunTask']);
function merge(iv) { iv.sort((a, b) => a[0] - b[0]); let s = 0, e = -1, sum = 0; for (const [a, b] of iv) { if (a > e) { sum += e - s; s = a; e = b; } else e = Math.max(e, b); } return sum + (e - s); }
export function analyse(events, tUp = null) {
  const names = {}, busy = {}, cnt = {}, dur = {}, timeline = [];
  let area = 0;
  if (tUp == null) { const u = events.find((e) => e.name === 'EventDispatch' && e.args && e.args.data && e.args.data.type === 'touchend'); tUp = u ? u.ts : events[0].ts; }
  for (const e of events) if (e.ph === 'M' && e.name === 'thread_name') names[e.pid + ':' + e.tid] = e.args.name;
  const pr = new Map(), frames = [];
  for (const e of events) {
    const tn = names[e.pid + ':' + e.tid] || '?';
    if (e.ph === 'X' && TOP.has(e.name)) (busy[tn] ||= []).push([e.ts, e.ts + (e.dur || 0)]);
    if (e.ph === 'X' && ['Paint', 'RasterTask', 'Layout', 'UpdateLayoutTree', 'PrePaint', 'Layerize', 'FireAnimationFrame', 'GPUTask', 'FunctionCall',
      'TimerFire', 'ProxyImpl::ScheduledActionCommit', 'Commit', 'Display::DrawAndSwap', 'SkiaOutputSurfaceImplOnGpu::SwapBuffers',
      'GpuChannel::ExecuteDeferredRequest', 'CommandBufferStub::OnAsyncFlush'].includes(e.name)) {
      cnt[e.name] = (cnt[e.name] || 0) + 1; dur[e.name] = (dur[e.name] || 0) + (e.dur || 0);
      const q = e.name === 'Paint' && e.args && e.args.data && e.args.data.clip;   // boyanan dörtgen (CSS px)
      const a = q && q.length >= 8 ? Math.min(873 * 393, Math.abs((q[2] - q[0]) * (q[5] - q[3]))) : 0;   // tavan: görüntü alanı
      area += a;
      if (tUp != null && ['Paint', 'RasterTask', 'FireAnimationFrame', 'Commit', 'Layerize'].includes(e.name))
        timeline.push([+((e.ts - tUp) / 1000).toFixed(1), e.name, Math.round(a), +((e.dur || 0) / 1000).toFixed(2), tn]);
    }
    if (e.name === 'PipelineReporter') {
      const k = e.pid + JSON.stringify(e.id2 || e.id);
      if (e.ph === 'b') { const fr = (e.args || {}).frame_reporter || {}; if (fr.frame_type !== 'FORKED') pr.set(k, [e.ts, fr.state]); }
      else if (e.ph === 'e' && pr.has(k)) { const [t, st] = pr.get(k); pr.delete(k); frames.push([(e.ts - t) / 1000, st]); }
    }
  }
  const thr = {};
  for (const [tn, iv] of Object.entries(busy)) { const k = tn.replace(/\d+$/, ''); thr[k] = (thr[k] || 0) + merge(iv) / 1000; }
  const want = frames.filter((f) => /PRESENTED_ALL|PRESENTED_PARTIAL|DROPPED/.test(f[1]));
  const drop = want.filter((f) => /DROPPED/.test(f[1])).length, slow = want.filter((f) => !/DROPPED/.test(f[1]) && f[0] > 16.7).length;
  const after = (n, a = 0, b = 400) => timeline.filter((x) => x[1] === n && x[0] >= a && x[0] <= b).length;
  const settle = { raster: after('RasterTask'), paint: after('Paint'), main_frames: after('Layerize'), raf: after('FireAnimationFrame') };
  return { settle, paint_kpx: Math.round(area / 1000), timeline: timeline.sort((a, b) => a[0] - b[0]), thread_ms: Object.fromEntries(Object.entries(thr).map(([k, v]) => [k, +v.toFixed(1)])), count: cnt,
    dur_ms: Object.fromEntries(Object.entries(dur).map(([k, v]) => [k, +(v / 1000).toFixed(1)])),
    frames: want.length, dropped: drop, slow_gt_16ms: slow, janky_pct: want.length ? +(100 * (drop + slow) / want.length).toFixed(1) : 0 };
}

const CATS = ['toplevel', 'cc', 'gpu', 'viz', 'benchmark', 'devtools.timeline', 'disabled-by-default-devtools.timeline.frame', 'blink'];
async function measure(root) {
  const args = ['--enable-gpu-rasterization', '--ignore-gpu-blocklist', '--disable-features=PaintHolding'];
  if (GPU === 'swiftshader') args.push('--use-angle=swiftshader', '--enable-unsafe-swiftshader');
  // chromium.launch = yeni süreç + geçici kullanıcı dizini → GPU program önbelleği her ölçümde soğuk
  const b = await chromium.launch({ executablePath: process.env.PW_CHROME, headless: true, args });
  const { ctx, page, cdp } = await open(b, root);
  const per = [];
  const seq = [];
  for (let r = 0; r < ROUNDS; r++) seq.push(+1, -1, -1, +1);   // System → sağ → geri → sol → geri
  const gpuInfo = await page.evaluate(() => { const c = document.createElement('canvas').getContext('webgl'); const d = c && c.getExtension('WEBGL_debug_renderer_info');
    return d ? c.getParameter(d.UNMASKED_RENDERER_WEBGL) : 'none'; });
  for (const d of seq) {
    await b.startTracing(page, { categories: CATS });
    const sw = await swipe(page, cdp, d);
    const tr = JSON.parse((await b.stopTracing()).toString());
    const a = analyse(tr.traceEvents || tr);
    if (per.length === 1 && TIMELINE) fs.writeFileSync(path.join(OUT, `timeline-${path.basename(root)}.json`), JSON.stringify(a.timeline));
    delete a.timeline;
    if (per.length === 1 && TIMELINE) fs.writeFileSync(path.join(OUT, `scrollpos-${path.basename(root)}.json`), JSON.stringify(sw.trace));
    delete sw.trace;
    per.push({ ...a, ...sw });
  }
  const landed = await page.evaluate(() => Math.round(document.getElementById('pager').scrollLeft / innerWidth));
  await ctx.close(); await b.close();
  return { gpu: gpuInfo, landed, per };
}

function sum(list, f) { return +list.reduce((a, x) => a + (f(x) || 0), 0).toFixed(1); }
function summary(per) {
  const pick = (k) => (x) => x.thread_ms[k];
  const s = (list) => ({ main_ms: sum(list, pick('CrRendererMain')), compositor_ms: sum(list, pick('Compositor')), gpu_ms: sum(list, pick('CrGpuMain')),
    viz_ms: sum(list, pick('VizCompositorThread')), tile_ms: sum(list, pick('CompositorTileWorker')),
    paint: sum(list, (x) => x.count.Paint), paint_kpx: sum(list, (x) => x.paint_kpx), raster: sum(list, (x) => x.count.RasterTask), raster_ms: sum(list, (x) => x.dur_ms.RasterTask),
    layout: sum(list, (x) => x.count.Layout), style: sum(list, (x) => x.count.UpdateLayoutTree), raf: sum(list, (x) => x.count.FireAnimationFrame),
    settle_raster: sum(list, (x) => x.settle.raster), settle_paint: sum(list, (x) => x.settle.paint), settle_main_frames: sum(list, (x) => x.settle.main_frames),
    settle_ms: sum(list, (x) => x.settle_ms), reversals: sum(list, (x) => x.reversals), off_page: sum(list, (x) => (x.on_page ? 0 : 1)),
    frames: sum(list, (x) => x.frames), dropped: sum(list, (x) => x.dropped), slow: sum(list, (x) => x.slow_gt_16ms) });
  const r = { first: s(per.slice(0, 1)), rest_per_swipe: Object.fromEntries(Object.entries(s(per.slice(1))).map(([k, v]) => [k, +(v / Math.max(1, per.length - 1)).toFixed(1)])) };
  const f = r.rest_per_swipe; f.janky_pct = f.frames ? +(100 * (f.dropped + f.slow) / f.frames).toFixed(1) : 0;
  return r;
}

async function main() {
  fs.mkdirSync(OUT, { recursive: true });
  const res = { rate: RATE, reps: REPS, rounds: ROUNDS, gpu: GPU, sys_page: SYS, viewport: '873x393@2.75', runs: {} };
  for (let k = 0; k < REPS; k++)
    for (const [name, dir] of (k % 2 ? [...VARIANTS].reverse() : VARIANTS)) {   // sıra dönüşümlü: yük kayması iki yana düşsün
      const m = await measure(path.resolve(dir));
      (res.runs[name] ||= []).push({ ...m, summary: summary(m.per) });
      console.log(k, name, m.gpu, 'landed', m.landed, JSON.stringify(summary(m.per)));
    }
  res.median = {};
  for (const [name, runs] of Object.entries(res.runs)) {
    const med = (sel) => { const v = runs.map(sel).sort((a, b) => a - b); return v[v.length >> 1]; };
    const keys = Object.keys(runs[0].summary.rest_per_swipe);
    res.median[name] = { rest_per_swipe: Object.fromEntries(keys.map((kk) => [kk, med((r) => r.summary.rest_per_swipe[kk])])),
      first: Object.fromEntries(Object.keys(runs[0].summary.first).map((kk) => [kk, med((r) => r.summary.first[kk])])) };
    console.log('medyan', name, JSON.stringify(res.median[name]));
  }
  const gate = arg('gate');
  if (gate) {   // TP-W3-12: yamalı panel kaydırırken upstream'den fazla boyama/raster/GPU işi yapmaz, oturma hızı (0001) korunur
    const u = res.median.upstream.rest_per_swipe, g = res.median[gate].rest_per_swipe;
    const checks = { raster: g.raster <= u.raster * 1.1, settle_raster: g.settle_raster <= u.settle_raster * 1.1, gpu_ms: g.gpu_ms <= u.gpu_ms * 1.1,
      settle_ms: g.settle_ms <= 300, off_page: res.runs[gate].every((r) => r.per.every((x) => x.on_page)) };
    res.gate = { name: gate, upstream: u, patched: g, checks, verdict: Object.values(checks).every(Boolean) ? 'PASS' : 'FAIL' };
    console.log('TP-W3-12', res.gate.verdict, JSON.stringify(checks), `raster ${u.raster}→${g.raster} settle_raster ${u.settle_raster}→${g.settle_raster} gpu_ms ${u.gpu_ms}→${g.gpu_ms} settle_ms ${g.settle_ms}`);
  }
  fs.writeFileSync(path.join(OUT, 'scroll.json'), JSON.stringify(res, null, 1));
  return res.gate && res.gate.verdict === 'FAIL' ? 1 : 0;
}
if (process.argv[1] && process.argv[1].endsWith('test_w3_scroll_pw.mjs')) main().then((rc) => process.exit(rc), (e) => { console.error(e); process.exit(2); });
