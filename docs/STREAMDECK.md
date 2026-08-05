# Stream Deck — subraum steuern

subraum bringt ein eigenes Stream-Deck-Plugin mit: Push-to-Talk halten, Mikro
stumm, Deafen, Funkkanäle, Lautstärke und Session-Steuerung — mit
**Live-Status auf den Tasten** (Sende-Anzeige, Stumm-Zustand, aktueller Kanal,
Lautstärke).

## Installation

1. subraum ≥ 0.3 installieren und einmal starten.
2. `subraum.streamDeckPlugin` aus dem GitHub-Release doppelklicken — die
   Stream-Deck-Software (ab Version 6.4) installiert es.
3. Tasten aus der Kategorie **subraum** aufs Deck ziehen. Fertig — keine
   Konfiguration, das Plugin findet die laufende App selbst.

Läuft subraum nicht, zeigen die Tasten „offline" und verbinden sich
automatisch, sobald die App startet.

## Tasten

| Taste | Funktion | Anzeige |
| --- | --- | --- |
| **Push-to-Talk** | Halten = senden (wie die PTT-Taste) | rot solange du sendest; „stumm" wenn Mikro stumm |
| **Mikro stumm** | Mikrofon an/aus | 🎙/🔇 + Zustand |
| **Deafen** | Ton komplett an/aus | 🔔/🔕 + Zustand |
| **Funkkanal wählen** | wechselt auf den eingestellten Kanal (Kanalname im Tasten-Inspektor) | leuchtet, wenn du auf dem Kanal bist |
| **Kanal vor / zurück** | durch die Kanalliste | aktueller Kanal |
| **Lauter / Leiser** | Gesamtlautstärke ±5 % | aktueller Wert |
| **Neu verschlüsseln** | raumweite Schlüsselrotation | „läuft…" während der Rotation |
| **Session verlassen** | trennt die Session | ausgegraut, wenn nicht verbunden |

Mikro-stumm gilt auch für Deck-PTT: Taste gedrückt + Mikro stumm = es wird
nichts gesendet (gleiche Logik wie in der App).

## Wie es technisch funktioniert

Die App öffnet beim Start eine **lokale Steuer-Schnittstelle** (WebSocket, nur
`127.0.0.1` — von außen nicht erreichbar). Zugriff erfordert ein Token, das bei
jedem App-Start neu erzeugt und hier abgelegt wird:

| System | Datei |
| --- | --- |
| Windows | `%APPDATA%\org.raumdock.subraum\control.json` |
| macOS | `~/Library/Application Support/org.raumdock.subraum/control.json` |
| Linux | `~/.config/org.raumdock.subraum/control.json` |

Format: `{"port": <Port>, "token": "<hex>"}`. Das Plugin liest die Datei und
verbindet sich; jedes andere lokale Werkzeug (Bitfocus Companion, Skripte) kann
dasselbe tun.

### Protokoll (für eigene Integrationen)

Erste Nachricht nach dem Verbinden: `{"t":"auth","token":"…"}`. Danach:

| Kommando | Wirkung |
| --- | --- |
| `{"t":"ptt","on":true/false}` | Push-to-Talk down/up |
| `{"t":"mic-toggle"}` | Mikro stumm umschalten |
| `{"t":"deafen-toggle"}` | Deafen umschalten |
| `{"t":"channel","name":"Funk 2"}` | Kanal direkt wechseln |
| `{"t":"chan-cycle","dir":1/-1}` | Kanal vor/zurück |
| `{"t":"volume","value":0..200}` | Gesamtlautstärke setzen (%) |
| `{"t":"volume-delta","d":±n}` | Gesamtlautstärke ändern |
| `{"t":"rekey"}` | Session neu verschlüsseln |
| `{"t":"disconnect"}` | Session verlassen |

Der Server antwortet nach der Auth mit
`{"t":"hello","app":"subraum","state":{…}}` und schickt bei jeder Änderung
`{"t":"state","state":{…}}`: `connected`, `transmitting`, `mic_muted`,
`deafened`, `channel`, `channels[]`, `volume`, `rekeying`.

## Plugin selbst bauen

```sh
cd streamdeck-plugin
npm install                                  # sharp für die Icons
node generate-icons.mjs                      # PNGs neu erzeugen (nur nach Änderung)
cd org.raumdock.subraum.sdPlugin && npm install --omit=dev && cd ..
node test-harness.mjs                        # Protokoll-Test ohne Hardware
powershell -ExecutionPolicy Bypass -File pack.ps1   # → subraum.streamDeckPlugin
```

Auf Linux/CI statt `pack.ps1`:
`cd streamdeck-plugin && zip -r subraum.streamDeckPlugin org.raumdock.subraum.sdPlugin`
