#!/usr/bin/env python3
"""phoned ölçümü (yalnız stdlib): bir süreç için N sn CPU + RSS, isteğe bağlı aynı sürede /api/stream istemcisi.

Kullanım: measure.py --pid P --port 8766 --secs 60 [--stream] [--label ad]
Çıktı: tek satır JSON — cpu_pct (bir çekirdeğin yüzdesi), rss_kb, threads, stream baytları/olay sayıları/en büyük boşluk.
CPU = /proc/<pid>/stat utime+stime farkı / (süre × CLK_TCK).
"""
import argparse, http.client, json, os, threading, time

def ticks(pid):
    with open(f"/proc/{pid}/stat") as f:
        rest = f.read().rsplit(")", 1)[1].split()
    return int(rest[11]) + int(rest[12])  # utime, stime (alan 14, 15)

def status(pid):
    out = {}
    with open(f"/proc/{pid}/status") as f:
        for line in f:
            k, _, v = line.partition(":")
            if k in ("VmRSS", "Threads"):
                out[k] = int(v.split()[0])
    return out

def stream(port, secs, res):
    c = http.client.HTTPConnection("127.0.0.1", port, timeout=secs + 10)
    c.request("GET", "/api/stream", headers={"Host": f"127.0.0.1:{port}"})
    r = c.getresponse()
    res["status"] = r.status
    end = time.monotonic() + secs
    buf, total, states, beats, other = b"", 0, [], [], 0
    writes = []
    while time.monotonic() < end:
        try:
            chunk = r.read1(65536)
        except Exception:
            break
        if not chunk:
            break
        now = time.monotonic()
        total += len(chunk); writes.append(now); buf += chunk
        while b"\n\n" in buf:
            ev, buf = buf.split(b"\n\n", 1)
            if ev.startswith(b"event: state"): states.append(now)
            elif ev == b":": beats.append(now)
            else: other += 1
    c.close()
    gap = lambda ts: round(max((b - a for a, b in zip(ts, ts[1:])), default=0.0), 3)
    res.update(bytes=total, bytes_per_s=round(total / secs, 2), states=len(states), heartbeats=len(beats), other_events=other,
               max_state_gap_s=gap(states), max_write_gap_s=gap(writes))

def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--pid", type=int, required=True); ap.add_argument("--port", type=int, default=8766)
    ap.add_argument("--secs", type=float, default=60); ap.add_argument("--stream", action="store_true"); ap.add_argument("--label", default="")
    a = ap.parse_args()
    hz = os.sysconf("SC_CLK_TCK")
    res = {"label": a.label}
    th = threading.Thread(target=stream, args=(a.port, a.secs, res)) if a.stream else None
    t0, c0 = time.monotonic(), ticks(a.pid)
    if th: th.start()
    time.sleep(a.secs)
    t1, c1 = time.monotonic(), ticks(a.pid)
    if th: th.join(a.secs + 15)
    st = status(a.pid)
    res.update(secs=round(t1 - t0, 1), cpu_pct=round(100.0 * (c1 - c0) / hz / (t1 - t0), 3), cpu_ticks=c1 - c0, rss_kb=st.get("VmRSS"), threads=st.get("Threads"))
    print(json.dumps(res, ensure_ascii=False))

if __name__ == "__main__":
    main()
