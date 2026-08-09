# Microsoft Store — listing copy (Subraum Communicator)

Ready-to-paste text for Partner Center → *Store listings*. Kept in the repo so
the wording is versioned alongside the release it describes.

Fill one listing per language. English is the default listing; German is the
second because that is the primary user base.

**Partner Center renders no Markdown.** Paste these blocks verbatim: `**bold**`
would show up as literal asterisks, so emphasis is done with plain uppercase
section labels and `•` bullets instead.

- **Product name (reserved):** `Subraum Communicator`
- **Privacy policy URL:** <https://subraum.cc/privacy>
- **Website:** <https://subraum.cc>
- **Support contact:** commercialusage@raumdock.org
- **Category:** Social → Communication (declare voice **and** text chat for the
  age rating questionnaire)

---

## English

### Short description

Push-to-talk voice and chat for a small squad. Audio goes straight from player
to player — no account, no recording, no media server in the middle.

### Description

```text
subraum is voice and text chat for small squads — with one difference: there is
no server listening in.

Audio goes straight from player to player. Every participant holds an encrypted
connection to every other one, and voice and chat travel only there. A small
service does nothing but introduce peers to each other: it brokers the
connection, keeps the roster and the session PIN, and then steps aside. It
cannot decrypt what is said afterwards.

Three steps, no configuration:
1. The host creates a session and gets a link and a six-digit PIN.
2. Teammates open the link and enter code and PIN.
3. The session stays alive as long as someone is connected.

VOICE
• Push-to-talk on any key, mouse button, mouse wheel or gamepad button — plus a
  toggle mode and an optional second key
• Self-mute and deafen
• Several radio channels per session, switched by global hotkey
• Optional in-game overlay showing the current channel

AUDIO
• Noise suppression, noise gate, compressor, limiter — each switchable
• Per-participant volume, free choice of microphone and output device
• Lowers other applications automatically while you speak
• Radio click so you can tell subraum apart from game audio
• Microphone self-test and a low-bandwidth mode for weak connections

CONNECTION
• Direct peer-to-peer, optional relay for strict NATs
• Automatic reconnect — losing the broker does not end the call
• Network self-check; connection type shown per participant

ENCRYPTION
• Voice over DTLS-SRTP, chat over DTLS-SCTP, brokering over TLS
• Plus a post-quantum key exchange (ML-KEM-768 with X25519)
• Keys live only for the session; re-encrypt at any time from the app
• No accounts, no recording, no history on any server

subraum uses your microphone for voice chat. Nothing is recorded.

Previously published as SquadLink Lite.
```

### What's new in this version

```text
Version 0.3.3 — a dead connection now tells you why.

• The "no connection to X" message names the cause: the other side never
  answered (a lost handshake, usually repaired by the automatic retry), or both
  sides exchanged addresses and still found no route (the path is blocked, as a
  strict router does). It lists the address types of both sides too.
```

Previous version, for reference:

```text
Version 0.3.2 — participants you could not reach are no longer silently mute.

• Fixed: if the direct connection to another participant never came up, or
  dropped later, the app neither noticed nor said so. You heard nothing from
  each other and nothing told you why. The app now spots the dead link, rebuilds
  the connection on its own, and marks the member NO CONNECTION until it is
  back.
• Fixed: channels you created did not show up for everyone. Same cause —
  channel announcements travel over that same direct connection — and it is
  fixed with it.
```

Previous version, for reference:

```text
Version 0.3.1 — a calmer window and hands-free transmit.

• The main window now shows only what you act on. The network bar and the
  encryption line are off by default; switch them back on under
  Settings › Expert › Interface if you want the numbers.
• New header: wordmark on the left, one row of icon buttons on the right —
  share, re-encrypt, microphone, sound, settings — then Leave and the
  connection dot. Green means connected, copper means transmitting.
• Link and PIN no longer sit on screen. The share button copies both to the
  clipboard, so there is nothing to hide while streaming.
• Hands-free transmit: hold the push-to-talk button longer than 0.7 seconds
  and it latches on until you press again. A quick double-tap of your
  push-to-talk key does the same. A short two-tone confirms it, on your
  headphones only.
• The push-to-talk button is smaller and centred; the old size is one switch
  away under Settings › Expert › Interface.
• Display size: pick 90 % to 150 % under Settings › Expert. It scales the whole
  interface, not just the text.
```

Previous version, for reference:

```text
Version 0.3.0 — Stream Deck, radio effect, and a fix for a silent-participant bug.

• Stream Deck integration: own Elgato plugin with live status on the keys —
  hold-to-talk, mute, deafen, channel select, volume, re-encrypt. Zero
  configuration; also usable from Bitfocus Companion via a local API.
• Radio effect (expert setting, off by default): incoming voices sound like a
  radio channel — band-pass, saturation, optional "broken handset" grit. Only
  affects what YOU hear; what you send stays clean.
• Fixed: closing the main window now really quits the app (the in-game overlay
  used to keep it alive in the background).
• Fixed: a misleading hint that recommended a relay which does not exist.
```

Previous version, for reference:

```text
Version 0.2.1 — fixes a bug that could leave one participant silent.

In some sessions a single participant could neither hear the others nor be heard
by them, while everyone else was fine. Whether it happened depended on who
joined when, so it looked random. The cause was in how the session's voice key
is handed out: someone joining later could end up holding a key nobody else had.

• Fixed: a participant is no longer left on a voice key of their own
• The app now tells you when it cannot obtain the voice key, instead of staying
  silently deaf
• Nothing changes in how you use it — sessions, links and settings are untouched

Older versions are affected too, so everyone in a session should update.
```

Previous version, for reference:

```text
Version 0.2.0 — the app is now called subraum.

An unrelated application already goes by "SquadLink". We renamed to rule out the
confusion. Same app, same team, same features — only the name, the icon and the
website are new.

• New name and new icon
• New website: subraum.cc
• Session links you already handed out keep working
• Encryption, settings and handling unchanged
```

### Search terms

`voice chat`, `push to talk`, `peer to peer`, `squad voice`, `gaming voice`,
`encrypted chat`, `subraum`

---

## Deutsch

### Kurzbeschreibung

Push-to-Talk-Sprache und Chat für eine kleine Crew. Der Ton läuft direkt von
Spieler zu Spieler — ohne Account, ohne Aufnahme, ohne Medienserver dazwischen.

### Beschreibung

```text
subraum ist Sprach- und Textchat für kleine Crews — mit einem Unterschied: Es
gibt keinen Server, der mithört.

Der Ton läuft direkt von Spieler zu Spieler. Jeder Teilnehmer hält eine
verschlüsselte Verbindung zu jedem anderen, und Sprache und Chat laufen
ausschließlich dort. Ein kleiner Dienst stellt die Teilnehmer einander nur vor:
Er vermittelt den Verbindungsaufbau, verwaltet Teilnehmerliste und Session-PIN
und tritt dann zur Seite. Was danach gesprochen wird, kann er nicht
entschlüsseln.

In drei Schritten drin, ohne Konfiguration:
1. Der Host erstellt eine Session und bekommt einen Link und eine sechsstellige
   PIN.
2. Die Mitspieler öffnen den Link und geben Code und PIN ein.
3. Die Session bleibt bestehen, solange jemand verbunden ist.

SPRACHE
• Push-to-Talk auf jeder Taste, Maustaste, dem Mausrad oder am Gamepad — dazu
  Umschaltmodus und optionale zweite Taste
• Selbst stummschalten und Ton komplett aus
• Mehrere Funkkanäle je Session, umschaltbar per globalem Hotkey
• Optionales Overlay im Spiel mit dem aktuellen Kanal

AUDIO
• Rauschunterdrückung, Noise Gate, Kompressor, Limiter — einzeln schaltbar
• Lautstärke je Teilnehmer, freie Wahl von Mikrofon und Ausgabegerät
• Senkt andere Anwendungen automatisch, während du sprichst
• Funk-Klick, damit du subraum vom Spielton unterscheidest
• Mikrofon-Selbsttest und Low-Bandwidth-Modus für schwache Leitungen

VERBINDUNG
• Direkt Peer-zu-Peer, optionaler Relay für strenge NATs
• Automatischer Reconnect — ein Abriss der Vermittlung beendet das Gespräch
  nicht
• Netzwerk-Selbsttest; Verbindungsart je Teilnehmer sichtbar

VERSCHLÜSSELUNG
• Sprache über DTLS-SRTP, Chat über DTLS-SCTP, Vermittlung über TLS
• Zusätzlich post-quanten-sicherer Schlüsselaustausch (ML-KEM-768 mit X25519)
• Schlüssel gelten nur für die Session; Neuverschlüsselung jederzeit per
  Knopfdruck
• Keine Accounts, keine Aufnahmen, kein Verlauf auf einem Server

subraum nutzt dein Mikrofon für den Sprachchat. Es wird nichts aufgezeichnet.

Früher veröffentlicht als SquadLink Lite.
```

### Neuerungen in dieser Version

```text
Version 0.3.3 — eine tote Verbindung sagt jetzt, woran es lag.

• Die Meldung „keine Verbindung zu X" nennt den Grund: Die Gegenstelle hat nie
  geantwortet (verlorener Verbindungsaufbau, den der automatische Neuaufbau
  meist repariert), oder beide Seiten haben Adressen getauscht und trotzdem kam
  keine Route zustande (der Weg ist blockiert, etwa durch einen strengen
  Router). Die Adresstypen beider Seiten stehen mit dabei.
```

Vorherige Version, zur Referenz:

```text
Version 0.3.2 — Teilnehmer ohne Verbindung stehen nicht mehr stumm in der Liste.

• Behoben: Kam die direkte Verbindung zu einem Teilnehmer nie zustande oder
  brach sie später weg, hat die App das weder gemerkt noch gesagt. Ihr habt
  euch gegenseitig nicht gehört, und nichts erklärte warum. Die App erkennt den
  toten Draht jetzt, baut die Verbindung selbstständig neu auf und schreibt bis
  dahin KEINE VERBINDUNG hinter den Namen.
• Behoben: Selbst angelegte Kanäle erschienen nicht bei allen. Gleiche Ursache —
  Kanal-Meldungen laufen über genau diese Direktverbindung — und damit mit
  erledigt.
```

Vorherige Version, zur Referenz:

```text
Version 0.3.1 — ein ruhigeres Fenster und Dauersenden ohne Halten.

• Das Hauptfenster zeigt nur noch, womit du auch etwas machst. Netz-Leiste und
  Verschlüsselungs-Zeile sind standardmäßig aus; unter
  Einstellungen › Experte › Oberfläche holst du die Werte zurück.
• Neue Kopfzeile: Wortmarke links, rechts eine Reihe Symbolknöpfe — teilen,
  neu verschlüsseln, Mikrofon, Ton, Einstellungen — dann „Verlassen" und der
  Verbindungspunkt. Grün heißt verbunden, kupfern heißt senden.
• Link und PIN stehen nicht mehr offen im Bild. Der Teilen-Knopf legt beides in
  die Zwischenablage, beim Streamen gibt es damit nichts mehr zu verbergen.
• Dauersenden ohne Halten: den Push-to-Talk-Knopf länger als 0,7 Sekunden
  halten rastet ihn ein, bis du erneut drückst. Ein schneller Doppeltipp auf
  deine Push-to-Talk-Taste macht dasselbe. Ein kurzer Zweiklang bestätigt es,
  nur auf deinen Kopfhörern.
• Der Push-to-Talk-Knopf ist kleiner und sitzt mittig; die alte Größe ist einen
  Schalter entfernt unter Einstellungen › Experte › Oberfläche.
• Darstellungsgröße: 90 % bis 150 % unter Einstellungen › Experte. Skaliert die
  gesamte Oberfläche, nicht nur die Schrift.
```

Vorherige Version, zur Referenz:

```text
Version 0.3.0 — Stream Deck, Funk-Effekt und ein Fix für stumme Teilnehmer.

• Stream-Deck-Integration: eigenes Elgato-Plugin mit Live-Status auf den
  Tasten — Push-to-Talk halten, Mikro, Deafen, Kanalwahl, Lautstärke, neu
  verschlüsseln. Null Konfiguration; über eine lokale Schnittstelle auch mit
  Bitfocus Companion nutzbar.
• Funk-Effekt (Experte, standardmäßig aus): eingehende Stimmen klingen wie
  Sprechfunk — Bandpass, Sättigung, auf Wunsch „kaputtes Handfunkgerät".
  Wirkt nur bei dir; was du sendest, bleibt unverändert.
• Behoben: das X des Hauptfensters beendet die App jetzt wirklich (das
  In-Game-Overlay hielt sie vorher im Hintergrund am Leben).
• Behoben: ein irreführender Hinweis empfahl ein Relay, das es nicht gibt.
```

Vorherige Version, zur Referenz:

```text
Version 0.2.1 — behebt einen Fehler, der einzelne Teilnehmer stumm ließ.

In manchen Sessions konnte ein einzelner Teilnehmer die anderen weder hören noch
von ihnen gehört werden, während der Rest normal funktionierte. Ob es auftrat,
hing davon ab, wer wann beitrat — es wirkte deshalb zufällig. Ursache war die
Verteilung des Sprach-Schlüssels: Wer später beitrat, konnte auf einem Schlüssel
landen, den sonst niemand hatte.

• Behoben: Teilnehmer landen nicht mehr auf einem eigenen Sprach-Schlüssel
• Die App meldet jetzt, wenn sie den Sprach-Schlüssel nicht bekommt, statt still
  taub zu bleiben
• An der Bedienung ändert sich nichts — Sessions, Links und Einstellungen
  bleiben unberührt

Ältere Versionen sind ebenfalls betroffen; alle Teilnehmer einer Session sollten
aktualisieren.
```

Vorherige Version, zur Referenz:

```text
Version 0.2.0 — die App heißt jetzt subraum.

Es gibt bereits eine andere Anwendung namens „SquadLink". Um Verwechslungen
auszuschließen, haben wir umbenannt. Gleiche App, gleiches Team, gleiche
Funktionen — neu sind nur Name, Icon und Website.

• Neuer Name und neues Icon
• Neue Website: subraum.cc
• Bereits verteilte Session-Links funktionieren weiter
• Verschlüsselung, Einstellungen und Bedienung unverändert
```

### Suchbegriffe

`Voice Chat`, `Push to Talk`, `Peer to Peer`, `Squad Voice`, `Gaming Voice`,
`verschlüsselt`, `subraum`

---

## Screenshots

Partner Center requires PNG at **1366×768 or larger**. The screenshots in
`server/init/assets/` are captures of the app window at its natural size
(largest is 959×830) and are **too small for the Store** — they are sized for
the website gallery, not for this.

Recapture for the Store with the window enlarged on a 1920×1080 display, or pad
the capture to 1366×768 on the app's background colour (`#0A0D13`) rather than
upscaling, which would soften the text.

Worth showing, in this order:

1. In a session — channels, roster, push-to-talk, chat
2. Start screen — host a session or join with link and PIN
3. Audio settings — microphone, push-to-talk, app ducking, radio click
4. Expert settings — overlay, channel hotkeys, re-encrypt, self-check

## Age rating

The questionnaire asks about user-to-user communication. Answer **yes** to both
voice and text chat: subraum carries live voice and a text chat between
participants. There is no moderation, no user-generated content stored on a
server, and no profile system.
