#!/usr/bin/env python3
"""Eşlik denetimi (W2.5): iki ajanı (A = .NET 0.6.9, B = phoned) aynı anda, YALNIZ yan etkisiz GET + SSE dinleme ile karşılaştırır.

Değer BASMAZ: yalnız anahtar yolu + tip. Ayar/eşleştirme sırları (lan.token, webhookToken, homeAssistant.token) yalnız
"iki uçta eşit mi" olarak raporlanır (sha256 karşılaştırması, özet bile basılmaz).

Sınıf: KIRMIZI = eksik/fazla anahtar ya da tip farkı · SARI = null↔değer, int↔float, boş↔dolu liste (çalışma anı verisi) ·
BİLEREK = PRD'de gerekçesi yazılı fark (--known dosyası, satır başına "uç yol").

Kullanım: parity.py --a 8765 --b 18766 [--sse 12] [--known docs/W2.5-known-diffs.txt] [--quick] [--md çıktı.md]
Çıkış: 0 = kırmızı yok · 1 = kırmızı var · 2 = uç erişilemedi.
"""
import argparse
import hashlib
import http.client
import json
import statistics
import sys
import threading
import time

ENDPOINTS = ["/api/pair", "/api/info", "/api/catalog", "/api/push", "/api/state", "/api/config", "/api/settings",
             "/api/approvals", "/api/quotas", "/api/work", "/api/procs", "/api/batteries", "/api/calendar", "/api/charge",
             "/api/phones", "/api/update", "/api/lan", "/api/obs", "/api/docker", "/api/ha/states"]
SECRETS = [("lan", "token"), ("lan", "webhookToken"), ("homeAssistant", "token")]
# Dizi elemanları tek bir şekil olarak karşılaştırılır; birleşim kümesi alınır (eleman sırası/sayısı çalışma anı verisi).


def get(port, path, timeout=5):
    c = http.client.HTTPConnection("127.0.0.1", port, timeout=timeout)
    try:
        c.request("GET", path, headers={"Host": f"127.0.0.1:{port}"})
        r = c.getresponse()
        body = r.read()
        return r.status, r.getheader("Content-Type") or "", body
    finally:
        c.close()


def tname(v):
    if v is None:
        return "null"
    if isinstance(v, bool):
        return "bool"
    if isinstance(v, int):
        return "int"
    if isinstance(v, float):
        return "float"
    if isinstance(v, str):
        return "str"
    if isinstance(v, list):
        return "list"
    return "obj"


def merge_items(items):
    """Dizi elemanlarından tek temsilci: nesnelerin anahtar birleşimi (ilk görülen değer)."""
    objs = [x for x in items if isinstance(x, dict)]
    if objs:
        out = {}
        for o in objs:
            for k, v in o.items():
                if k not in out or out[k] is None:
                    out[k] = v
        return out
    return items[0] if items else None


def compare(a, b, path, rows, depth=0):
    ta, tb = tname(a), tname(b)
    if ta != tb:
        if "null" in (ta, tb):
            rows.append(("SARI", path or "/", f"{ta} ↔ {tb}"))
        elif {ta, tb} == {"int", "float"}:
            rows.append(("SARI", path or "/", f"{ta} ↔ {tb}"))
        else:
            rows.append(("KIRMIZI", path or "/", f"tip {ta} ↔ {tb}"))
        return
    if ta == "obj":
        for k in sorted(set(a) | set(b)):
            p = f"{path}.{k}" if path else k
            if k not in b:
                rows.append(("KIRMIZI", p, "B'de yok"))
            elif k not in a:
                rows.append(("KIRMIZI", p, "B'de fazla"))
            elif depth < 12:
                compare(a[k], b[k], p, rows, depth + 1)
    elif ta == "list":
        if bool(a) != bool(b):
            rows.append(("SARI", path + "[]", f"{len(a)} ↔ {len(b)} eleman"))
        elif a and b and depth < 12:
            compare(merge_items(a), merge_items(b), path + "[]", rows, depth + 1)


def secret_eq(ja, jb):
    out = []
    for sect, key in SECRETS:
        va = (ja or {}).get(sect, {}) if isinstance(ja, dict) else {}
        vb = (jb or {}).get(sect, {}) if isinstance(jb, dict) else {}
        va = va.get(key) if isinstance(va, dict) else None
        vb = vb.get(key) if isinstance(vb, dict) else None
        h = lambda v: hashlib.sha256(json.dumps(v).encode()).digest()
        out.append((f"{sect}.{key}", "ikisi de boş" if va in (None, "") and vb in (None, "") else ("EŞİT" if h(va) == h(vb) else "FARKLI")))
    return out


def sse(port, secs, res):
    """`secs` sn dinle: olay türü → [(zaman, bayt)], ilk state gövdesi."""
    ev = {}
    first_state = None
    t0 = time.monotonic()
    try:
        c = http.client.HTTPConnection("127.0.0.1", port, timeout=secs + 5)
        c.request("GET", "/api/stream", headers={"Host": f"127.0.0.1:{port}", "Accept": "text/event-stream"})
        r = c.getresponse()
        # Okuma zaman aşımı BufferedReader'ı bozar (sonraki okumalar boş döner) → engelleyici oku; iki uç da en geç
        # 4 sn'de bir çerçeve/kalp atışı yazar, süre her okumadan sonra denetlenir (taşma ≤ 4 sn).
        c.sock.settimeout(None)
        buf = b""
        while time.monotonic() - t0 < secs:
            try:
                chunk = r.read1(65536)
            except OSError:
                break
            if not chunk:
                break
            buf += chunk
            while b"\n\n" in buf:
                frame, buf = buf.split(b"\n\n", 1)
                now = time.monotonic() - t0
                name, data = "message", b""
                for line in frame.split(b"\n"):
                    if line.startswith(b":"):
                        name = name if data else ":"
                    elif line.startswith(b"event: "):
                        name = line[7:].decode()
                    elif line.startswith(b"data: "):
                        data += line[6:]
                if name == "message" and not data:
                    name = ":"
                ev.setdefault(name, []).append((now, len(frame) + 2))
                if name == "state" and first_state is None:
                    try:
                        first_state = json.loads(data)
                    except ValueError:
                        pass
        c.close()
    except OSError as e:
        res["error"] = str(e)
    res["events"] = ev
    res["first_state"] = first_state


def gaps(ts):
    g = [b - a for a, b in zip(ts, ts[1:])]
    if not g:
        return "—"
    return f"{min(g):.2f}/{statistics.median(g):.2f}/{max(g):.2f}"


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--a", type=int, default=8765)
    ap.add_argument("--b", type=int, default=18766)
    ap.add_argument("--sse", type=int, default=12)
    ap.add_argument("--known", default="")
    ap.add_argument("--quick", action="store_true", help="SSE yok, yalnız özet satırı")
    ap.add_argument("--md", default="")
    o = ap.parse_args()
    known = set()
    if o.known:
        try:
            for line in open(o.known, encoding="utf-8"):
                line = line.split("#", 1)[0].strip()
                if line:
                    known.add(line)
        except OSError:
            pass

    sse_res = {"a": {}, "b": {}}
    threads = []
    if not o.quick and o.sse > 0:
        for side, port in (("a", o.a), ("b", o.b)):
            t = threading.Thread(target=sse, args=(port, o.sse, sse_res[side]))
            t.start()
            threads.append(t)

    out = []
    table = []
    reds = yellows = knowns = 0
    cfg = {}
    unreachable = False
    for ep in ENDPOINTS:
        try:
            sa, ca, ba = get(o.a, ep)
            sb, cb, bb = get(o.b, ep)
        except OSError as e:
            table.append((ep, "KIRMIZI", "—", f"erişilemedi: {e.__class__.__name__}"))
            unreachable = True
            continue
        rows = []
        if sa != sb:
            rows.append(("KIRMIZI", "HTTP", f"{sa} ↔ {sb}"))
        try:
            ja = json.loads(ba) if ba else None
        except ValueError:
            ja = None
        try:
            jb = json.loads(bb) if bb else None
        except ValueError:
            jb = None
        if sa == 200 and sb == 200:
            compare(ja, jb, "", rows)
        if ep in ("/api/config", "/api/settings"):
            cfg[ep] = (ja, jb)
        for cls, path, what in rows:
            key = f"{ep} {path}"
            if cls == "KIRMIZI" and key in known:
                cls = "BİLEREK"
            if cls == "KIRMIZI":
                reds += 1
            elif cls == "SARI":
                yellows += 1
            else:
                knowns += 1
            table.append((ep, cls, path, what))
        if not rows:
            table.append((ep, "EŞ", "—", f"{sa}"))

    for t in threads:
        t.join()

    out.append(f"# Eşlik A=127.0.0.1:{o.a} B=127.0.0.1:{o.b} · {time.strftime('%F %T')}")
    out.append(f"KIRMIZI {reds} · SARI {yellows} · BİLEREK {knowns}")
    out.append("")
    out.append("| uç | sınıf | yol | fark |")
    out.append("|---|---|---|---|")
    for ep, cls, path, what in table:
        out.append(f"| `{ep}` | {cls} | `{path}` | {what} |")

    if "/api/settings" in cfg:
        ja, jb = cfg["/api/settings"]
        out.append("")
        out.append("## Ayar sürekliliği (değer basılmaz)")
        same = json.dumps(ja, sort_keys=True) == json.dumps(jb, sort_keys=True)
        out.append(f"- `/api/settings` gövdesi iki uçta birebir: **{'EVET' if same else 'HAYIR'}**")
        if not same and isinstance(ja, dict) and isinstance(jb, dict):
            diffk = sorted(k for k in set(ja) | set(jb) if json.dumps(ja.get(k), sort_keys=True) != json.dumps(jb.get(k), sort_keys=True))
            out.append(f"- değeri farklı üst anahtarlar: {', '.join(diffk) or '—'}")
        for name, verdict in secret_eq(ja, jb):
            out.append(f"- `{name}`: {verdict}")

    if threads:
        out.append("")
        out.append(f"## SSE ({o.sse} sn, aynı anda)")
        out.append("| taraf | olay | adet | bayt | aralık min/medyan/maks sn | ilk (sn) |")
        out.append("|---|---|---|---|---|---|")
        for side in ("a", "b"):
            r = sse_res[side]
            if r.get("error"):
                out.append(f"| {side.upper()} | HATA | — | — | {r['error']} | — |")
            for name, lst in sorted(r.get("events", {}).items()):
                ts = [t for t, _ in lst]
                out.append(f"| {side.upper()} | `{name}` | {len(lst)} | {sum(b for _, b in lst)} | {gaps(ts)} | {ts[0]:.2f} |")
        ta = set(sse_res["a"].get("events", {})) - {":"}
        tb = set(sse_res["b"].get("events", {})) - {":"}
        out.append(f"- olay türleri A={sorted(ta)} B={sorted(tb)}")
        rows = []
        if sse_res["a"].get("first_state") is not None and sse_res["b"].get("first_state") is not None:
            compare(sse_res["a"]["first_state"], sse_res["b"]["first_state"], "", rows)
        sred = [r for r in rows if r[0] == "KIRMIZI" and f"/api/stream {r[1]}" not in known]
        out.append(f"- SSE `state` şekli: KIRMIZI {len(sred)} · SARI {sum(1 for r in rows if r[0] == 'SARI')}")
        for cls, path, what in rows:
            k = "BİLEREK" if cls == "KIRMIZI" and f"/api/stream {path}" in known else cls
            out.append(f"  - {k} `{path}` {what}")
        reds += len(sred)
        if tb - ta - {"work"} or ta - tb:
            out.append(f"- KIRMIZI: olay türü farkı A−B={sorted(ta - tb)} B−A={sorted(tb - ta)}")

    text = "\n".join(out)
    if o.md:
        with open(o.md, "w", encoding="utf-8") as f:
            f.write(text + "\n")
    if o.quick:
        print(f"eşlik: KIRMIZI {reds} · SARI {yellows} · BİLEREK {knowns}")
    else:
        print(text)
    sys.exit(2 if unreachable else (1 if reds else 0))


if __name__ == "__main__":
    main()
