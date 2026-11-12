#!/usr/bin/env python3
"""TP-W2-11 dayanıklılık: bir phoned süreci için RSS/CPU zaman serisi (yalnız /proc okuma + yan etkisiz GET/SSE).

İstemci yükü (panel önizlemesinin eşi): sürekli açık `/api/stream` (EventSource gibi) + 5 sn'de bir `/api/config` ve
`/api/push` (panelin yenileme okumaları) + 60 sn'de bir `/panel/` ve `/api/catalog`. Tarayıcı yalnız MemAvailable ≥ 4 GB
iken kullanılabilir (COMMON bellek kapısı); her örnekte MemAvailable yazılır.

Kullanım: soak.py --pid P --port 18766 --secs 3600 --every 30 --out soak.tsv
Çıktı: TSV (t_s, rss_kb, cpu_s, cpu_pct_aralık, threads, fds, sse_bayt, sse_state, memavail_mb) + özet satırı.
"""
import argparse
import http.client
import os
import threading
import time

TICK = os.sysconf("SC_CLK_TCK")


def proc(pid):
    with open(f"/proc/{pid}/status") as f:
        st = {k: v.strip() for k, v in (line.split(":", 1) for line in f if ":" in line)}
    with open(f"/proc/{pid}/stat") as f:
        parts = f.read().rsplit(")", 1)[1].split()
    cpu = (int(parts[11]) + int(parts[12])) / TICK
    return int(st["VmRSS"].split()[0]), cpu, int(st["Threads"]), len(os.listdir(f"/proc/{pid}/fd"))


def memavail_mb():
    with open("/proc/meminfo") as f:
        for line in f:
            if line.startswith("MemAvailable:"):
                return int(line.split()[1]) // 1024
    return -1


class Sse(threading.Thread):
    def __init__(self, port, stop):
        super().__init__(daemon=True)
        self.port, self.stop, self.bytes, self.states, self.reconnects = port, stop, 0, 0, 0

    def run(self):
        while not self.stop.is_set():
            try:
                c = http.client.HTTPConnection("127.0.0.1", self.port, timeout=30)
                c.request("GET", "/api/stream", headers={"Host": f"127.0.0.1:{self.port}", "Accept": "text/event-stream"})
                r = c.getresponse()
                while not self.stop.is_set():
                    chunk = r.read1(65536)
                    if not chunk:
                        break
                    self.bytes += len(chunk)
                    self.states += chunk.count(b"event: state\n")
            except OSError:
                pass
            self.reconnects += 1
            time.sleep(1)


def poll(port, stop):
    n = 0
    while not stop.is_set():
        paths = ["/api/config", "/api/push"] + (["/panel/", "/api/catalog"] if n % 12 == 0 else [])
        for p in paths:
            try:
                c = http.client.HTTPConnection("127.0.0.1", port, timeout=5)
                c.request("GET", p, headers={"Host": f"127.0.0.1:{port}"})
                c.getresponse().read()
                c.close()
            except OSError:
                pass
        n += 1
        stop.wait(5)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--pid", type=int, required=True)
    ap.add_argument("--port", type=int, default=18766)
    ap.add_argument("--secs", type=int, default=3600)
    ap.add_argument("--every", type=int, default=30)
    ap.add_argument("--out", required=True)
    o = ap.parse_args()
    stop = threading.Event()
    sse = Sse(o.port, stop)
    sse.start()
    threading.Thread(target=poll, args=(o.port, stop), daemon=True).start()
    t0 = time.monotonic()
    rows = []
    prev_cpu = None
    with open(o.out, "w") as f:
        f.write("t_s\trss_kb\tcpu_s\tcpu_pct\tthreads\tfds\tsse_bytes\tsse_states\tmemavail_mb\n")
        while True:
            t = time.monotonic() - t0
            try:
                rss, cpu, thr, fds = proc(o.pid)
            except OSError:
                f.write(f"{t:.0f}\tSÜREÇ YOK\n")
                break
            pct = 0.0 if prev_cpu is None else 100 * (cpu - prev_cpu[1]) / max(t - prev_cpu[0], 1e-9)
            prev_cpu = (t, cpu)
            rows.append((t, rss, cpu))
            f.write(f"{t:.0f}\t{rss}\t{cpu:.2f}\t{pct:.3f}\t{thr}\t{fds}\t{sse.bytes}\t{sse.states}\t{memavail_mb()}\n")
            f.flush()
            if t >= o.secs:
                break
            time.sleep(o.every)
        stop.set()
        if len(rows) >= 2:
            (t_a, r_a, c_a), (t_b, r_b, c_b) = rows[0], rows[-1]
            # büyüme: ilk 10 dk'dan sonraki en düşük → son (ısınma hariç)
            warm = [r for t, r, _ in rows if t >= 600] or [r_b]
            f.write(
                f"# özet: süre {t_b - t_a:.0f} sn · RSS {r_a / 1024:.1f} → {r_b / 1024:.1f} MB (en yüksek {max(r for _, r, _ in rows) / 1024:.1f}, "
                f"ısınma sonrası min {min(warm) / 1024:.1f}) · CPU ort %{100 * (c_b - c_a) / max(t_b - t_a, 1e-9):.3f} · "
                f"SSE {sse.bytes} B / {sse.states} state / yeniden bağlanma {sse.reconnects - 0}\n"
            )


if __name__ == "__main__":
    main()
