"""Sahte adb sunucusu (gölge koşu için; gerçek 5037'ye ve telefona dokunulmaz).
Kullanım: python3 -I tests/fake_adb.py <port> <kayıt.jsonl>  — tek cihaz `FAKE0`, paket `net.oxog.phonebox.deskpanel` kurulu.
Her servis isteği kayda bir satır yazar (ts, serial, servis). track-devices bağlantısı açık tutulur."""
import json, socket, sys, threading, time

PORT, LOG = int(sys.argv[1]), sys.argv[2]
rules = set()
lock = threading.Lock()


def log(serial, svc):
    with lock, open(LOG, "a") as f:
        f.write(json.dumps({"ts": time.time(), "serial": serial, "svc": svc}) + "\n")


def hexed(s):
    b = s.encode()
    return b"%04x" % len(b) + b


def read_req(c):
    n = c.recv(4, socket.MSG_WAITALL)
    if len(n) < 4:
        return None
    return c.recv(int(n, 16), socket.MSG_WAITALL).decode() if int(n, 16) else ""


def handle(c):
    try:
        first = read_req(c)
        if first == "host:track-devices":
            log("-", first)
            c.sendall(b"OKAY" + hexed("FAKE0\tdevice\n"))
            while c.recv(1):
                pass
            return
        if not first or not first.startswith("host:transport:"):
            c.sendall(b"FAIL" + hexed("unknown"))
            return
        serial = first.split(":", 2)[2]
        c.sendall(b"OKAY")
        svc = read_req(c)
        log(serial, svc)
        c.sendall(b"OKAY")
        if svc == "reverse:list-forward":
            c.sendall(b"OKAY" + hexed("".join(f"{serial} {r}\n" for r in sorted(rules))))
        elif svc.startswith("reverse:forward:"):
            rules.add(svc.split(":", 2)[2].replace(";", " "))
            c.sendall(b"OKAY")
        elif svc.startswith("shell:pm path net.oxog.phonebox.deskpanel"):
            c.sendall(b"package:/data/app/fake/base.apk\n")
    finally:
        c.close()


s = socket.socket()
s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
s.bind(("127.0.0.1", PORT))
s.listen(8)
while True:
    conn, _ = s.accept()
    threading.Thread(target=handle, args=(conn,), daemon=True).start()
