#!/usr/bin/env bash
# W2.4b ses mutantları (docs/TEST-POINTS.md W24b-M1–M4). Filoda, temiz ağaçta: bash tests/w24b_mutants.sh
set -u
run() { # ad dosya eski yeni test-süzgeci
  cp "$2" /tmp/mut.bak
  python3 -I -c "import sys;p,o,n=sys.argv[1:];s=open(p).read();assert o in s,('YOK',o);open(p,'w').write(s.replace(o,n,1))" "$2" "$3" "$4" || { echo "$1 UYGULANAMADI"; return; }
  if cargo nextest run --locked -E "test(/$5/)" >/tmp/mut.log 2>&1; then echo "$1 HAYATTA ($5)"; else echo "$1 ÖLDÜ ($5): $(grep -m1 -E 'FAIL ' /tmp/mut.log | sed 's/ \+/ /g')"; fi
  cp /tmp/mut.bak "$2"
}
cargo nextest run --locked >/dev/null 2>&1 && echo "TABAN YEŞİL"
run W24b-M1 src/audio/proto.rs 'let max = volumes.iter().copied().max().unwrap_or(0);' 'let max = volumes.first().copied().unwrap_or(0);' tp_w24b_a2
run W24b-M2 src/audio/mod.rs 'mute: (level > 0 && sink.mute).then_some(false)' 'mute: None' tp_w24b_a
run W24b-M3 src/audio/mod.rs 'self.tick.send_modify(|t| *t = t.wrapping_add(1));' '' tp_w24b_a6_external
run W24b-M4 src/audio/mod.rs 'pub const RETRY_MS: u64 = 5_000;' 'pub const RETRY_MS: u64 = 0;' tp_w24b_a7_no_socket
cargo nextest run --locked >/dev/null 2>&1 && echo "GERİ YÜKLEME SONRASI YEŞİL"
