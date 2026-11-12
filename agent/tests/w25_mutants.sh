#!/usr/bin/env bash
# W2.5 mutantları (docs/TEST-POINTS.md W25-M1–M8, W25-G1–G5). Filoda, temiz ağaçta: bash tests/w25_mutants.sh
set -u
run() { # ad dosya eski yeni test-süzgeci
  cp "$2" /tmp/mut.bak
  python3 -I -c "import sys;p,o,n=sys.argv[1:];s=open(p).read();assert o in s,('YOK',o);open(p,'w').write(s.replace(o,n,1))" "$2" "$3" "$4" || { echo "$1 UYGULANAMADI"; return; }
  if cargo nextest run --locked -E "test(/$5/)" >/tmp/mut.log 2>&1; then echo "$1 HAYATTA ($5)"; else echo "$1 ÖLDÜ ($5): $(grep -m1 -E 'FAIL ' /tmp/mut.log | sed 's/ \+/ /g')"; fi
  cp /tmp/mut.bak "$2"
}
cargo nextest run --locked >/dev/null 2>&1 && echo "TABAN YEŞİL" || { echo "TABAN KIRMIZI — mutant koşulmaz"; exit 1; }
run W25-M1 src/app.rs '() = panel_changed(media) => {}' '() = std::future::pending::<()>() => {}' tp_w25_m1
run W25-M2 src/media/mod.rs 'prev.is_none_or(|p| p.props != props)' 'true' tp_w25_m2
run W25-M3 src/media/mod.rs 'g.remove(bus).is_some()' '{ g.remove(bus); true }' tp_w25_m2
run W25-M4 src/app.rs 'if !adb_given && a.port != PRODUCTION_PORT {' 'if false {' tp_w25_a1
run W25-M5 src/app.rs 'if !adb_given && a.port != PRODUCTION_PORT {' 'if a.port != PRODUCTION_PORT {' tp_w25_a1
run W25-M6 src/compat.rs '.and_then(Value::as_bool).unwrap_or(false);' '.and_then(Value::as_bool).map(|_| false).unwrap_or(false);' tp_w25_s1
run W25-M7 src/compat.rs '.unwrap_or_default();
    json_ok(&Value::Array(list))' '.unwrap_or_default();
    let _ = list;
    json_ok(&Value::Array(Vec::new()))' tp_w25_s1
run W25-M8 src/app.rs '.merge(crate::compat::routes());' ';' tp_w2_01_shapes
run W25-G1 src/gpu/mod.rs '"vramUsed": used.map(|v| v / 1024.0),' '"vramUsed": used,' tp_w25_g1
run W25-G2 src/gpu/mod.rs 'deadline = Instant::now() + gpu.settings.idle_stop;
            }' '}' tp_w25_g2
run W25-G3 src/gpu/mod.rs 'End::Missing => return,' 'End::Missing => tokio::time::sleep(gpu.settings.retry).await,' tp_w25_g3
run W25-G4 src/gpu/mod.rs 'let end = session(&gpu, &mut rx, &mut name).await;
        gpu.set(None);' 'let end = session(&gpu, &mut rx, &mut name).await;' tp_w25_g2
run W25-G5 src/hub.rs 'gpu: self.gpu.as_ref().and_then(|g| g.snapshot()),' 'gpu: None,' tp_w25_g2
cargo nextest run --locked >/dev/null 2>&1 && echo "GERİ YÜKLEME SONRASI YEŞİL"
