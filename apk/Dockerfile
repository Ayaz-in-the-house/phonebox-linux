# phone-panel-apk — PhoneBox APK derleme kabı (yalnız buildbox'da kurulur; host'a paket kurulmaz).
# Tüm sürümler tools/apk/versions.lock'tan --build-arg ile gelir; burada varsayılan YOK (kilit dışı derleme olmaz).
# apt kullanılmaz: zip açma JDK'nin `jar` aracıyla (ek paket = ek sabitlenmemiş sürüm olurdu).
ARG BASE_IMAGE
FROM ${BASE_IMAGE}
ARG CMDLINE_TOOLS_ZIP
ARG CMDLINE_TOOLS_SHA256
ARG CMDLINE_TOOLS_SHA1
ARG SDK_PACKAGES
ARG SDK_PLATFORM_REVISION
ENV ANDROID_HOME=/opt/android-sdk ANDROID_SDK_ROOT=/opt/android-sdk
SHELL ["/bin/bash", "-euo", "pipefail", "-c"]
RUN test -n "${CMDLINE_TOOLS_ZIP}" && test -n "${CMDLINE_TOOLS_SHA256}" && test -n "${CMDLINE_TOOLS_SHA1}" ; \
    curl -fsSL --proto '=https' --tlsv1.2 -o /tmp/ct.zip "https://dl.google.com/android/repository/${CMDLINE_TOOLS_ZIP}" ; \
    echo "${CMDLINE_TOOLS_SHA256}  /tmp/ct.zip" | sha256sum -c - ; \
    echo "${CMDLINE_TOOLS_SHA1}  /tmp/ct.zip" | sha1sum -c - ; \
    mkdir -p /tmp/ct "${ANDROID_HOME}/cmdline-tools" ; cd /tmp/ct ; jar xf /tmp/ct.zip ; \
    mv /tmp/ct/cmdline-tools "${ANDROID_HOME}/cmdline-tools/latest" ; chmod +x "${ANDROID_HOME}"/cmdline-tools/latest/bin/* ; \
    rm -rf /tmp/ct /tmp/ct.zip
ENV PATH=/opt/android-sdk/cmdline-tools/latest/bin:${PATH}
# sdkmanager her paketi repository2-3.xml'deki sha1 ile doğrular; kurulan sürüm kilitle karşılaştırılır (TP-APK-02).
RUN (yes || true) | sdkmanager --licenses >/dev/null ; \
    sdkmanager --install ${SDK_PACKAGES} >/dev/null ; \
    sdkmanager --list_installed 2>/dev/null | tee /opt/android-sdk/INSTALLED.txt ; \
    for p in ${SDK_PACKAGES}; do grep -q "^ *${p} " /opt/android-sdk/INSTALLED.txt || { echo "eksik: $p"; exit 1; }; done ; \
    grep -Eq "^ *platforms;android-35 +\| +${SDK_PLATFORM_REVISION} " /opt/android-sdk/INSTALLED.txt ; \
    chmod -R a+rX /opt/android-sdk
ENV PATH=/opt/android-sdk/build-tools/34.0.0:${PATH}
COPY in-container.sh /usr/local/bin/in-container.sh
RUN chmod 755 /usr/local/bin/in-container.sh
WORKDIR /src
