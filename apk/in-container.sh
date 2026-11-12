#!/bin/bash
# phone-panel-apk kabının içindeki tek giriş: fetch | build | keygen | certsha
# /src = yamalı kaynak (rw) · /out = çıktı · /ks = anahtar dizini (build'de ro) · /gradle = Gradle önbelleği
# Parola hiçbir zaman komut satırına ya da çıktıya yazılmaz: keytool -storepass:file, Gradle'a ortam değişkeni kap içinde dosyadan.
set -euo pipefail
mode="${1:?mode}"
export GRADLE_USER_HOME=/gradle HOME=/tmp/home
mkdir -p "$HOME"
ALIAS=phone-panel
G=(./gradlew --no-daemon --console=plain -Pandroid.builder.sdkDownload=false)

wrapper_check() {
  cd /src/android
  echo "${GRADLE_WRAPPER_JAR_SHA256:?}  gradle/wrapper/gradle-wrapper.jar" | sha256sum -c - >/dev/null
  grep -q "^distributionSha256Sum=${GRADLE_DIST_SHA256:?}$" gradle/wrapper/gradle-wrapper.properties
}

case "$mode" in
  keygen)
    test ! -e /ks/release.p12 || { echo "anahtar zaten var: üzerine yazılmaz" >&2; exit 5; }
    umask 077
    head -c 32 /dev/urandom | base64 | tr -d '/+=\n' > /ks/store.pass
    keytool -genkeypair -keystore /ks/release.p12 -storetype PKCS12 -storepass:file /ks/store.pass \
      -alias "$ALIAS" -keyalg RSA -keysize 4096 -validity 10000 -dname "CN=phone-panel-local, O=home" >/dev/null 2>&1
    keytool -exportcert -keystore /ks/release.p12 -storepass:file /ks/store.pass -alias "$ALIAS" 2>/dev/null \
      | sha256sum | cut -d' ' -f1 > /ks/cert.sha256
    cat /ks/cert.sha256 ;;
  fetch)
    wrapper_check
    extra=()
    [ -f gradle/verification-metadata.xml ] || extra=(--write-verification-metadata sha256)
    # Anahtar bağlanmaz → upstream debug anahtarına düşer; bu APK atılır, yalnız önbelleği doldurur.
    "${G[@]}" --info "${extra[@]}" :app:assembleRelease > /out/fetch.log 2>&1 || { tail -40 /out/fetch.log; exit 6; }
    cp gradle/verification-metadata.xml /out/verification-metadata.xml
    cp /opt/android-sdk/INSTALLED.txt /out/sdk-installed.txt ;;
  build)
    wrapper_check
    test -f gradle/verification-metadata.xml || { echo "verification-metadata yok: önce fetch" >&2; exit 7; }
    test -s /ks/release.p12 && test -s /ks/store.pass && test -s /ks/cert.sha256 || { echo "anahtar yok: debug anahtarına DÜŞÜLMEZ" >&2; exit 8; }
    PHONEBOX_STORE_PASSWORD="$(cat /ks/store.pass)"
    export PHONEBOX_STORE_PASSWORD PHONEBOX_KEY_PASSWORD="$PHONEBOX_STORE_PASSWORD"
    export PHONEBOX_STORE_FILE=/ks/release.p12 PHONEBOX_KEY_ALIAS="$ALIAS"
    "${G[@]}" --offline clean :app:assembleRelease > /out/build.log 2>&1 || { tail -40 /out/build.log; exit 6; }
    unset PHONEBOX_STORE_PASSWORD PHONEBOX_KEY_PASSWORD
    apk=app/build/outputs/apk/release/app-release.apk
    name="PhoneBox-${UPSTREAM_VERSION:?}-${SERIES:?}.apk"
    cp "$apk" "/out/$name"
    cd /out
    apksigner verify --verbose --print-certs "$name" > apksigner.txt
    got=$(sed -n 's/^Signer #1 certificate SHA-256 digest: //p' apksigner.txt)
    want=$(cat /ks/cert.sha256)
    [ "$got" = "$want" ] || { echo "imza bizim anahtarımız DEĞİL ($got)" >&2; exit 9; }
    grep -q '^Verifies$' apksigner.txt
    sha256sum "$name" > SHA256SUMS ;;
  *) echo "bilinmeyen kip: $mode" >&2; exit 2 ;;
esac
