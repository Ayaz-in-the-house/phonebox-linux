#!/usr/bin/env bash
# W2.3 mutantları (docs/TEST-POINTS.md M8–M11). Filoda, temiz ağaçta: bash tests/w23_mutants.sh
set -u
run() { # ad dosya eski yeni test-süzgeci
  cp "$2" /tmp/mut.bak
  python3 -I -c "import sys;p,o,n=sys.argv[1:];s=open(p).read();assert o in s,('YOK',o);open(p,'w').write(s.replace(o,n,1))" "$2" "$3" "$4" || { echo "$1 UYGULANAMADI"; return; }
  if cargo nextest run --locked -E "test(/$5/)" >/tmp/mut.log 2>&1; then echo "$1 HAYATTA ($5)"; else echo "$1 ÖLDÜ ($5): $(grep -m1 -E 'FAIL ' /tmp/mut.log | sed 's/ \+/ /g')"; fi
  cp /tmp/mut.bak "$2"
}
cargo nextest run --locked >/dev/null 2>&1 && echo "TABAN YEŞİL"
run M8 src/adb/mod.rs '(Some(s), Some("device")) if' '(Some(s), Some(_)) if' tp_w2_07
run M9 src/actions/mod.rs 'if !looking(&st) {' 'if false {' tp_w2_08bc
run M10 src/adb/mod.rs 'format!("{pkg}/{ACTIVITY_CLASS}")' 'format!("{pkg}/.MainActivity")' tp_w2_07
run M11 src/zen/mod.rs 'let can = can_route(pid);' 'let can = can_route(pid) && false;' tp_w2_09
cargo nextest run --locked >/dev/null 2>&1 && echo "GERİ YÜKLEME SONRASI YEŞİL"
