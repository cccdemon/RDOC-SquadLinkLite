# RDOC SquadLink Lite — Linux

Installation, Voraussetzungen und Fehlerbehebung für die Linux-Version.
Unterstützt **Ubuntu/Debian**, **Fedora**, **Arch**, **SteamOS / Steam Deck**
und die gängigen Gaming-Distributionen.

Alle Builds sind **unsigniert** (Prototyp). Prüfsummen: siehe
[Prüfsummen verifizieren](#prüfsummen-verifizieren).
Download-Übersicht: **https://squadlink.raumdock.org/get**

---

## Unterstützte Systeme

| Distribution / Familie | Empfohlenes Format | Architektur |
| --- | --- | --- |
| Ubuntu, Debian, Linux Mint, Pop!_OS | `.deb` | amd64, arm64 |
| Fedora, Nobara (klassisch) | `.rpm` | x86_64, aarch64 |
| Arch, Manjaro, Garuda, EndeavourOS | `.AppImage` | x86_64, arm64 |
| **SteamOS / Steam Deck** | **Flatpak** | x86_64 |
| Bazzite, ChimeraOS, Nobara atomic (immutable) | **Flatpak** | x86_64 |
| Beliebige Distribution (Fallback) | `.AppImage` | x86_64, arm64 |

> **Immutable / Read-only-Systeme** (SteamOS, Bazzite, ChimeraOS): `.deb`/`.rpm`
> lassen sich dort nicht sauber installieren. **Flatpak** ist der native Weg,
> `.AppImage` funktioniert als portabler Fallback.

---

## Laufzeit-Voraussetzungen

- **64-bit** x86_64 oder arm64/aarch64.
- **WebKitGTK 4.1** (`libwebkit2gtk-4.1-0`) + **GTK 3** (`libgtk-3-0`) — die GUI
  läuft in einem WebKit-Webview. (`.AppImage` und Flatpak bringen WebKit selbst
  mit, brauchen also kein System-WebKit.)
- **ALSA** (`libasound2`) bzw. **PipeWire/PulseAudio** — für Mikrofon und
  Wiedergabe. Moderne Distributionen (inkl. SteamOS) nutzen PipeWire.
- **Mikrofonzugriff** — Voice-App; ohne Mikro kein Senden.
- Netzwerk: ausgehend zum Signaling-Server (`wss://squadlink.raumdock.org`) und
  P2P/STUN zu den Mitspielern. Kein eingehender Port nötig.

Opus ist statisch in die Binary gelinkt — kein `libopus`-Laufzeitpaket nötig.

---

## Installation

### Debian / Ubuntu / Mint / Pop!_OS (`.deb`)

```sh
sudo apt install ./RDOC\ SquadLink\ Lite_*_amd64.deb
# oder, falls Abhängigkeiten fehlen:
sudo dpkg -i ./RDOC\ SquadLink\ Lite_*_amd64.deb && sudo apt -f install
```

### Fedora / Nobara (`.rpm`)

```sh
sudo dnf install ./RDOC\ SquadLink\ Lite-*.x86_64.rpm
```

### Arch / Manjaro / beliebige Distribution (`.AppImage`)

```sh
chmod +x ./RDOC\ SquadLink\ Lite_*_amd64.AppImage
./RDOC\ SquadLink\ Lite_*_amd64.AppImage
```

Braucht **FUSE 2** (`libfuse2`). Fehlt sie: `sudo apt install libfuse2` bzw.
mit `--appimage-extract-and-run` starten.

### SteamOS / Steam Deck & immutable Distributionen (Flatpak)

```sh
# Einmalig Flathub-Runtime-Quelle (falls nicht vorhanden):
flatpak remote-add --if-not-exists flathub https://flathub.org/repo/flathub.flatpakrepo

# App aus dem Bundle installieren:
flatpak install --user ./rdoc-squadlink-lite.flatpak

# Start:
flatpak run org.raumdock.SquadLinkLite
```

**Steam Deck:** im **Desktop-Modus** installieren (oben). Für den **Gaming-Modus**
die App in Steam als „Nicht-Steam-Spiel hinzufügen" aufnehmen (Ziel:
`flatpak run org.raumdock.SquadLinkLite`).

---

## Push-to-Talk unter Linux

Globales Push-to-Talk (Taste wird auch erkannt, während ein Vollbild-Spiel im
Vordergrund ist) gibt es aktuell **nur unter Windows**. Unter Linux funktioniert
der **In-App-PTT-Button** sowie PTT, solange das App-Fenster den Fokus hat.
Toggle-Senden, Selbst-Stummschaltung und Deafen funktionieren überall.

---

## Fehlerbehebung

**Weißes / leeres Fenster beim Start (Wayland, moderne GPUs).** WebKitGTK 2.42+
nutzt standardmäßig einen DMABUF-Renderer, der auf vielen Treibern fehlschlägt.
Die App setzt `WEBKIT_DISABLE_DMABUF_RENDERER=1` und
`WEBKIT_DISABLE_COMPOSITING_MODE=1` selbst, falls nicht bereits gesetzt — bleibt
das Fenster trotzdem leer, manuell exportieren:

```sh
WEBKIT_DISABLE_DMABUF_RENDERER=1 WEBKIT_DISABLE_COMPOSITING_MODE=1 <start-befehl>
```

**Kein Ton / Mikro geht nicht (Flatpak).** Die App bekommt Audio über den
PulseAudio-/PipeWire-Socket. Fehlt das Mikro trotzdem, Berechtigung prüfen:

```sh
flatpak info --show-permissions org.raumdock.SquadLinkLite
# Notlösung (breiter Geräte-Zugriff), falls ALSA direkt gebraucht wird:
flatpak override --user --device=all org.raumdock.SquadLinkLite
```

**`squadlink://`-Links öffnen die App nicht.** Bei `.deb`/`.rpm` registriert die
App das Schema beim ersten Start über `xdg`. Prüfen/erzwingen:

```sh
xdg-mime query default x-scheme-handler/squadlink
update-desktop-database ~/.local/share/applications
```

**AppImage startet nicht.** FUSE 2 installieren (siehe oben) oder mit
`--appimage-extract-and-run` starten.

---

## Prüfsummen verifizieren

Jeder Download hat eine `.sha256`-Datei; pro Release liegt zusätzlich eine
`SHA256SUMS-linux-<arch>.txt` bei.

```sh
sha256sum -c RDOC\ SquadLink\ Lite_*_amd64.deb.sha256
# oder gegen die Sammel-Datei:
sha256sum -c SHA256SUMS-linux-amd64.txt
```

---

## Aus dem Quellcode bauen

Build-Voraussetzungen (Ubuntu 22.04/24.04):

```sh
sudo apt-get install -y \
  libwebkit2gtk-4.1-dev libgtk-3-dev libayatana-appindicator3-dev \
  librsvg2-dev libasound2-dev libopus-dev libssl-dev \
  patchelf file build-essential curl xdg-utils desktop-file-utils
```

Zusätzlich **Rust** (stable) und **Node 20 + pnpm 10**. Dann:

```sh
cd apps/companion
pnpm install --frozen-lockfile
pnpm tauri build --bundles deb,rpm,appimage   # oder: pnpm tauri dev
```

Die Bundles landen unter
`apps/companion/src-tauri/target/release/bundle/`.

> **Hinweis:** `libopus-dev` und `clang`/`build-essential` sind zwingend — sonst
> bricht der `audiopus`-Build mit „Failed to autogen Opus" ab.

Die CI baut alle Formate reproduzierbar auf sauberen Runnern
(`.github/workflows/build-companion-linux.yml` für deb/rpm/AppImage,
`build-companion-flatpak.yml` für das Flatpak-Bundle).
