"""TP-APK-04 — Gradle --info günlüğündeki indirme adresleri yalnız resmî kaynaklar mı.
python3 -I hosts_check.py <gradle.log> → çıkış 0 izinli · 1 izin dışı host var · 2 günlükte indirme yok (denetlenemedi)."""
import re, sys

ALLOWED = {
    "dl.google.com", "maven.google.com",                                  # Google Maven (AGP, androidx)
    "repo.maven.apache.org", "repo1.maven.org",                           # Maven Central (Kotlin)
    "services.gradle.org", "downloads.gradle.org", "downloads.gradle-dn.com",  # Gradle dağıtımı
    "plugins.gradle.org", "plugins-artifacts.gradle.org",                 # Gradle eklenti portalı (pluginManagement)
}
URL = re.compile(r"\b(?:Download(?:ing)?|Downloading from|Resource missing|Cached resource \S+ is up-to-date)\b[^\n]*?https?://([A-Za-z0-9.-]+)")


def main(path):
    hosts = {}
    for line in open(path, encoding="utf-8", errors="replace"):
        for m in URL.finditer(line):
            hosts[m.group(1).lower()] = hosts.get(m.group(1).lower(), 0) + 1
    if not hosts:
        print("DENETLENEMEDİ: günlükte indirme satırı yok")
        return 2
    bad = sorted(h for h in hosts if h not in ALLOWED)
    for h in sorted(hosts):
        print(f"{'İZİNLİ' if h in ALLOWED else 'İZİN DIŞI'}\t{h}\t{hosts[h]}")
    return 1 if bad else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1]))
