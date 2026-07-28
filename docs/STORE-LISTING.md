# Microsoft Store — listing copy (Subraum Communicator)

Ready-to-paste text for Partner Center → *Store listings*. Kept in the repo so
the wording is versioned alongside the release it describes.

Fill one listing per language. English is the default listing; German is the
second because that is the primary user base.

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

subraum is a serverless voice mesh for small groups. Every participant holds a
direct encrypted connection to every other participant, and voice and chat
travel only there. A small signaling service introduces peers to each other and
keeps the roster and session PIN; it relays the handshake and then steps aside.
It cannot decrypt what follows.

Getting a squad in takes three steps and no configuration: the host creates a
session and gets a link and a six-digit PIN, teammates open the link and enter
code and PIN, and the session stays alive while members are connected.

**Voice**
- Push-to-talk on any key, mouse button, mouse wheel or gamepad button, plus a
  toggle mode and an optional second key
- Self-mute and deafen
- Multiple radio channels per session, switchable by global hotkey
- Optional in-game overlay showing the current channel

**Audio quality**
- Noise suppression, noise gate, compressor and limiter, each switchable
- Per-participant volume and free choice of input and output device
- Microphone self-test with local playback
- Automatically lowers other applications while you speak (Windows)
- Optional radio click so you can tell subraum apart from game audio
- Low-bandwidth mode for weak connections

**Connection**
- Direct peer-to-peer by default; optional TURN relay for strict NATs
- Automatic reconnect with backoff — a signaling drop does not end the call
- Network self-check for send, receive, STUN and signaling
- Connection type shown per participant (direct or relay)

**Encryption**
- Voice over DTLS-SRTP, chat over DTLS-SCTP, signaling over TLS
- Additionally sealed with a post-quantum hybrid handshake (ML-KEM-768 with
  X25519, ChaCha20-Poly1305)
- Keys are ephemeral per session; a room-wide re-key can be triggered at any
  time from the app
- No accounts, no recording, no message history on any server

**Uses your microphone** for voice chat. Nothing is recorded and no audio
reaches the signaling service.

Previously published as SquadLink Lite. The app was renamed to subraum because
an unrelated application already goes by "SquadLink"; the rename rules out the
confusion. Same app, same team — existing Store installs update normally.

### What's new in this version

Version 0.2.0 — the app is now called subraum (previously SquadLink Lite). An
unrelated application already goes by "SquadLink", so the rename rules out the
confusion. Same app, same team.

- New name, new icon, new website at subraum.cc
- Session links you already handed out keep working
- Everything else is unchanged: same encryption, same features, same settings

### Search terms

`voice chat`, `push to talk`, `peer to peer`, `squad voice`, `gaming voice`,
`encrypted chat`, `subraum`

---

## Deutsch

### Kurzbeschreibung

Push-to-Talk-Sprache und Chat für eine kleine Crew. Der Ton läuft direkt von
Spieler zu Spieler — ohne Account, ohne Aufnahme, ohne Medienserver dazwischen.

### Beschreibung

subraum ist ein serverloses Voice-Mesh für kleine Gruppen. Jeder Teilnehmer
hält eine direkte verschlüsselte Verbindung zu jedem anderen, und Sprache und
Chat laufen ausschließlich dort. Ein kleiner Signaling-Dienst stellt die
Teilnehmer einander vor und verwaltet Teilnehmerliste und Session-PIN; er
vermittelt den Handshake und tritt dann zur Seite. Was danach läuft, kann er
nicht entschlüsseln.

Eine Crew ist in drei Schritten drin, ohne Konfiguration: Der Host erstellt eine
Session und erhält einen Link und eine sechsstellige PIN, die Mitspieler öffnen
den Link und geben Code und PIN ein, und die Session bleibt bestehen, solange
Teilnehmer verbunden sind.

**Sprache**
- Push-to-Talk auf jeder Taste, Maustaste, dem Mausrad oder einer Gamepad-Taste,
  dazu ein Umschaltmodus und eine optionale zweite Taste
- Selbst stummschalten und Ton komplett aus
- Mehrere Funkkanäle je Session, umschaltbar per globalem Hotkey
- Optionales In-Game-Overlay mit dem aktuellen Kanal

**Audioqualität**
- Rauschunterdrückung, Noise Gate, Kompressor und Limiter, einzeln schaltbar
- Lautstärke je Teilnehmer, freie Wahl von Eingabe- und Ausgabegerät
- Mikrofon-Selbsttest mit Eigenwiedergabe
- Senkt automatisch die Lautstärke anderer Anwendungen, während du sprichst
  (Windows)
- Optionaler Funk-Klick, damit du subraum vom Spielton unterscheiden kannst
- Low-Bandwidth-Modus für schwache Verbindungen

**Verbindung**
- Standardmäßig direkt Peer-zu-Peer; optionaler TURN-Relay für strenge NATs
- Automatischer Reconnect mit Backoff — ein Signaling-Abriss beendet das
  Gespräch nicht
- Netzwerk-Selbsttest für Senden, Empfangen, STUN und Signaling
- Verbindungsart je Teilnehmer sichtbar (direkt oder Relay)

**Verschlüsselung**
- Sprache über DTLS-SRTP, Chat über DTLS-SCTP, Signaling über TLS
- Zusätzlich abgesichert durch einen post-quanten-sicheren Handshake (ML-KEM-768
  mit X25519, ChaCha20-Poly1305)
- Schlüssel sind pro Session flüchtig; eine room-weite Neuverschlüsselung lässt
  sich jederzeit in der App auslösen
- Keine Accounts, keine Aufnahmen, kein Nachrichtenverlauf auf einem Server

**Nutzt dein Mikrofon** für den Sprachchat. Es wird nichts aufgezeichnet, und
kein Ton erreicht den Signaling-Dienst.

Früher veröffentlicht als SquadLink Lite. Die App wurde in subraum umbenannt,
weil bereits eine andere Anwendung den Namen „SquadLink" trägt; die Umbenennung
schließt Verwechslungen aus. Gleiche App, gleiches Team — bestehende
Store-Installationen aktualisieren sich ganz normal.

### Neuerungen in dieser Version

Version 0.2.0 — die App heißt jetzt subraum (vorher SquadLink Lite). Es gibt
bereits eine andere Anwendung namens „SquadLink"; die Umbenennung schließt
Verwechslungen aus. Gleiche App, gleiches Team.

- Neuer Name, neues Icon, neue Website unter subraum.cc
- Bereits verteilte Session-Links funktionieren weiter
- Sonst unverändert: gleiche Verschlüsselung, gleiche Funktionen, gleiche
  Einstellungen

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
