set -u
run() { # ad dosya eski yeni test
  cp "$2" /tmp/mut.bak
  python3 -I -c "import sys;p,o,n=sys.argv[1:];s=open(p).read();assert o in s,('YOK',o);open(p,'w').write(s.replace(o,n,1))" "$2" "$3" "$4" || { echo "$1 UYGULANAMADI"; return; }
  if cargo nextest run --locked -E "test(/$5/)" >/tmp/mut.log 2>&1; then echo "$1 HAYATTA ($5)"; else echo "$1 ÖLDÜ ($5): $(grep -m1 -E 'FAIL ' /tmp/mut.log | sed 's/ \+/ /g')"; fi
  cp /tmp/mut.bak "$2"
}
cargo nextest run --locked >/dev/null 2>&1 && echo "TABAN YEŞİL"
run M1 src/guard.rs 'Some("cross-site" | "same-site")' 'Some("cross-site")' tp_w2_02
run M2 src/config.rs '&& ticks(cur) != exp' '&& false && ticks(cur) != exp' tp_w2_03
run M3 src/stream.rs '(x - y).abs() > 1,' '(x - y).abs() > 0,' tp_w2_06
run M4 src/stream.rs 'HEARTBEAT_MS: u64 = 4_000' 'HEARTBEAT_MS: u64 = 9_000' tp_w2_05
run M5 src/config.rs 'let options = if kind == "command" {' 'let options = if kind == "commandX" {' tp_w2_03g
run M6 src/hub.rs 'self.subscribers() > 0 ||' 'true ||' tp_w2_s2
run M7 src/stream.rs 'Out::State
        } else if' 'Out::Heartbeat
        } else if' tp_w2_13
# ADR-11 sessiz kip zaman tavanı (K3)
run M8 src/stream.rs 'QUIET_STATE_MS: u64 = 30_000' 'QUIET_STATE_MS: u64 = 1_000' tp_w2_05
run M9 src/stream.rs 'self.dirty |= bits != 0;' 'self.dirty = bits != 0;' tp_w2_05_quiet_busy
run M10 src/stream.rs '|| first_data ||' '||' tp_w2_05_first
run M11 src/stream.rs 'mode == Mode::Live || first_data' 'first_data' tp_w2_05_unlock
run M12 src/stream.rs 'self.pending |= bits;' 'self.pending = bits;' tp_w2_05_first
run M13 src/stream.rs 'if cur.is_some() || now_ms' 'if true || now_ms' tp_w2_05_first_client
# K7 Zen zinciri (TP-AP-19..22)
run K7-M1 src/zen/mod.rs 'let tail = end.min(k - 1);' 'let tail = 0;' tp_ap_19_mark_across
run K7-M2 src/zen/mod.rs '            return Ok((true, total));
        }
        let tail' '            if false { return Ok((true, total)); }
        }
        let tail' tp_ap_19
run K7-M3 src/zen/mod.rs '    drop(s);
    Ok(can_route(pid).await.0)' '    let mut s = s;
    let _ = s.write_all(b"\n").await;
    drop(s);
    Ok(can_route(pid).await.0)' tp_ap_20
run K7-M4 src/zen/mod.rs '    drop(s);
    Ok(can_route(pid).await.0)' '    drop(s);
    let _ = pid;
    Ok(true)' tp_ap_20
run K7-M5 src/herdr/mod.rs '&& self.routed.map(|r| r.0) == Some(routed_at)' '' tp_ap_22
run K7-M6 src/herdr/mod.rs 'self.routed = Some((routed_at, route_ms));' '' tp_ap_22
# K8 medya (Zen position state + YouTube kaynağı)
run K8-M1 src/media/mod.rs '        p.length_us = o.props.length_us;' '' tp_k8_m1
run K8-M2 src/media/mod.rs 'let anchor = if p.length_us > 0 {' 'let anchor = if true {' tp_k8_m3
run K8-M3 src/media/mod.rs 'if a.playing == playing { a }' 'if true { a }' tp_k8_m4
run K8-M4 src/media/mod.rs '!key.is_empty() && track_key(&o.props) == key' 'true' tp_k8_m2
run K8-Y1 src/media/yt.rs 'same_video(url, &s.id) || !playing' '!playing' tp_k8_y3
run K8-Y2 src/media/yt.rs '> DRIFT_MS' '> 0' tp_k8_y5
run K8-Y3 src/media/yt.rs 'self.queue.truncate(QUEUE_MAX);' '' tp_k8_y1
run K8-Y4 src/media/yt.rs 'if !self.paused {' 'if true {' tp_k8_y2
run K8-Y5 src/media/yt.rs 'while a.len() > ART_KEEP {' 'while a.len() > 1000 {' tp_k8_y4
run K8-Y6 src/media/mod.rs 'Some(y) if yt::prefer(' 'Some(y) if true || yt::prefer(' tp_k8_y3
run K8-Y7 src/media/yt.rs 'self.bump(used);' 'self.bump(false);' tp_k8_y5
# K11 dikte (TP-DT-01..08): her davranış satırına ≥1 mutant
run DT-M1 src/dictate/http.rs 'if !wav_type(&h) {' 'if false {' tp_dt_02_rejects
run DT-M2 src/dictate/mod.rs 'if (format, channels, rate, bits) != (Some(1), Some(1), Some(16_000), Some(16)) {' 'if false {' tp_dt_02
run DT-M3 src/dictate/mod.rs 'if samples == 0 {' 'if false {' tp_dt_02
run DT-M4 src/dictate/mod.rs 'if b.get(0..4) != Some(b"RIFF") || b.get(8..12) != Some(b"WAVE") {' 'if false {' tp_dt_02_check_wav
run DT-M5 src/dictate/mod.rs '.saturating_add(size & 1);' ';' tp_dt_02_check_wav
run DT-M6 src/dictate/http.rs 'to_bytes(body, MAX_BYTES)' 'to_bytes(body, usize::MAX)' tp_dt_03
run DT-M7 src/dictate/mod.rs 'if status != 200 {' 'if false {' tp_dt_04_engine_error
run DT-M8 src/dictate/mod.rs 'if host != "127.0.0.1" && !host.eq_ignore_ascii_case("localhost") {' 'if false {' tp_dt_04
run DT-M9 src/dictate/mod.rs 'Authorization: Bearer {token}' 'Authorization: Bearer x{token}' tp_dt_01_forwards
run DT-M10 src/dictate/mod.rs 'if text.is_empty() || !paste {' 'if !paste {' tp_dt_05
run DT-M11 src/dictate/mod.rs 'if text.is_empty() || !paste {' 'if text.is_empty() {' tp_dt_06_paste_0
run DT-M12 src/dictate/mod.rs 'let text = text.trim().to_owned();' 'let text = text.clone();' tp_dt_05
run DT-M13 src/dictate/mod.rs 'Ok(Ok(s)) if s.success() => Ok(()),' 'Ok(Ok(_)) => Ok(()),' tp_dt_06_paste_failure
run DT-M14 src/dictate/mod.rs 'let paste_error = self.paste(&text).await.err();' 'let _ = self.paste(&text).await; let paste_error = self.paste(&text).await.err();' tp_dt_01_forwards
run DT-M15 src/dictate/mod.rs 'let _lane = self.lane.lock().await;' '' tp_dt_07_concurrent
run DT-M16 src/dictate/mod.rs '>= MAX_QUEUE {' '>= usize::MAX {' tp_dt_07_queue
run DT-M17 src/dictate/http.rs 'text.chars().count(),' 'text,' tp_dt_08_log
run DT-M18 src/dictate/mod.rs 'let token = token.trim().to_owned();' 'let token = token.clone();' tp_dt_01_token_file
run DT-M19 src/dictate/mod.rs 'None => std::fs::read_to_string(&self.settings.url_file)' 'None => Ok::<_, std::io::Error>("http://127.0.0.1:1".to_owned())' tp_dt_01_forwards
run DT-M20 src/dictate/http.rs 'Some("0") => false,' 'Some("0") => true,' tp_dt_06_paste_0
cargo nextest run --locked >/dev/null 2>&1 && echo "GERİ YÜKLEME SONRASI YEŞİL"
