#!/usr/bin/env bash
# W2.4 mutantları (docs/TEST-POINTS.md W24-M1–M5; w24 dalında elle koşulmuştu, K4'te betiğe bağlandı).
# Filoda, temiz ağaçta: bash tests/w24_mutants.sh
set -u
run() { # ad dosya eski yeni test-süzgeci
  cp "$2" /tmp/mut.bak
  python3 -I -c "import sys;p,o,n=sys.argv[1:];s=open(p).read();assert o in s,('YOK',o);open(p,'w').write(s.replace(o,n,1))" "$2" "$3" "$4" || { echo "$1 UYGULANAMADI"; return; }
  if cargo nextest run --locked -E "test(/$5/)" >/tmp/mut.log 2>&1; then echo "$1 HAYATTA ($5)"; else echo "$1 ÖLDÜ ($5): $(grep -m1 -E 'FAIL ' /tmp/mut.log | sed 's/ \+/ /g')"; fi
  cp /tmp/mut.bak "$2"
}
cargo nextest run --locked >/dev/null 2>&1 && echo "TABAN YEŞİL"
run W24-M1 src/kill/mod.rs 'if let Some(n) = name_denied(p) {' 'if let Some(n) = name_denied(p).filter(|_| false) {' tp_w24_k1
run W24-M2 src/kill/mod.rs 'pub const GRACE_MS: u64 = 5_000;' 'pub const GRACE_MS: u64 = 0;' tp_ex_04d
run W24-M3 src/procs/mod.rs 'pub const CACHE_MS: u64 = 1_000;' 'pub const CACHE_MS: u64 = 0;' tp_ex_05
run W24-M4 src/kill/http.rs '.is_none_or(|c| c.0.ip().is_loopback())' '.is_none_or(|_| true)' tp_ex_04e
run W24-M5 src/kill/linux.rs 'if !same_identity(pid, start) {' 'if false {' wrong_identity
cargo nextest run --locked >/dev/null 2>&1 && echo "GERİ YÜKLEME SONRASI YEŞİL"
