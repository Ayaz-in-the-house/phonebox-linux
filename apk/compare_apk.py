"""TP-APK-06/08 — iki APK'yı içerik olarak kıyaslar (imza dosyaları hariç).
python3 -I compare_apk.py --ref <upstream.apk> --apk <bizim.apk> [--www-tree <yamalı www dizini>] [--json <çıktı>]
Ölçüt: dosya listesi aynı · www dışı içerik aynı · --www-tree yoksa assets/www upstream'le aynı (yalnız CRLF→LF farkı ayrı
raporlanır, içerik farkı sayılmaz), varsa ağaçla bayt bayt aynı (upstream'den farklılar raporlanır).
Çıkış 0 geçer · 1 fark var."""
import argparse, hashlib, json, os, re, sys, zipfile

SIG = re.compile(r"^META-INF/([^/]+\.(SF|RSA|DSA|EC|MF)|com/android/metadata)$", re.I)
WWW = "assets/www/"


def entries(path):
    with zipfile.ZipFile(path) as z:
        return {i.filename: hashlib.sha256(z.read(i)).hexdigest() for i in z.infolist()
                if not i.is_dir() and not SIG.match(i.filename)}


def eol_only(ref_apk, apk, names):
    """Yalnız CR farkı olanlar (upstream sürüm APK'sı Windows çalışma ağacından: CRLF; depo LF)."""
    with zipfile.ZipFile(ref_apk) as a, zipfile.ZipFile(apk) as b:
        return sorted(n for n in names if a.read(n).replace(b"\r\n", b"\n") == b.read(n).replace(b"\r\n", b"\n"))


def tree(path):
    out = {}
    for d, _, fs in os.walk(path):
        for f in fs:
            p = os.path.join(d, f)
            out[WWW + os.path.relpath(p, path).replace(os.sep, "/")] = hashlib.sha256(open(p, "rb").read()).hexdigest()
    return out


def main(argv=None):
    a = argparse.ArgumentParser()
    a.add_argument("--ref", required=True)
    a.add_argument("--apk", required=True)
    a.add_argument("--www-tree")
    a.add_argument("--json")
    o = a.parse_args(argv)
    ref, apk = entries(o.ref), entries(o.apk)
    www = lambda m: {k: v for k, v in m.items() if k.startswith(WWW)}
    r = {
        "only_ref": sorted(set(ref) - set(apk)),
        "only_apk": sorted(set(apk) - set(ref)),
        "www_files": len(www(apk)),
        "www_differs_from_ref": sorted(k for k in www(apk) if k in ref and ref[k] != apk[k]),
        "other_differs_from_ref": sorted(k for k in apk if k in ref and ref[k] != apk[k] and not k.startswith(WWW)),
    }
    r["www_eol_only"] = eol_only(o.ref, o.apk, r["www_differs_from_ref"])
    r["www_content_differs_from_ref"] = sorted(set(r["www_differs_from_ref"]) - set(r["www_eol_only"]))
    ok = not r["only_ref"] and not r["only_apk"] and not r["other_differs_from_ref"]
    if o.www_tree:
        t = tree(o.www_tree)
        r["www_vs_tree"] = sorted(k for k in set(t) | set(www(apk)) if t.get(k) != www(apk).get(k))
        ok = ok and not r["www_vs_tree"]
    else:
        ok = ok and not r["www_content_differs_from_ref"]
    r["result"] = "GEÇTİ" if ok else "FARK"
    s = json.dumps(r, ensure_ascii=False, indent=1)
    print(s)
    if o.json:
        open(o.json, "w").write(s + "\n")
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())
