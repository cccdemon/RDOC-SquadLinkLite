# Datenschutzerklärung — subraum

_Stand: 2026-06-11_

subraum ("die App") ist eine Desktop-Anwendung für verschlüsselte
Peer-to-Peer-Sprach- und Textkommunikation. Diese Erklärung beschreibt, welche Daten
die App verarbeitet.

## Verantwortlicher

Raumdock (RDOC) — Kontakt: <kontakt@raumdock.org>

## Welche Daten verarbeitet werden

- **Anzeigename**: der von dir eingegebene Name. Wird an die Mitspieler deiner
  Sitzung übertragen, damit sie sehen, wer spricht. Nicht serverseitig gespeichert.
- **Mikrofon-Audio**: wird nur während einer aktiven Sitzung erfasst, Ende-zu-Ende
  verschlüsselt (DTLS-SRTP) direkt an die anderen Teilnehmer übertragen und **nicht
  aufgezeichnet oder gespeichert**.
- **Chat-Nachrichten**: Ende-zu-Ende verschlüsselt (DTLS-SCTP) peer-to-peer, nicht
  serverseitig gespeichert.
- **Verbindungs-Metadaten**: Sitzungscode, eine zufällige temporäre Teilnehmer-ID und
  technische Verbindungsdaten (IP-Adressen) werden kurzzeitig vom Signaling-Server
  verarbeitet, um die direkte P2P-Verbindung herzustellen. Sie werden nach Ende der
  Sitzung nicht dauerhaft gespeichert.

## Server

- **Signaling-Server** (`subraum.cc`): vermittelt den Verbindungsaufbau.
  Sieht keine entschlüsselten Audio- oder Chat-Inhalte.
- **Update-Prüfung**: die App fragt die öffentliche GitHub-Releases-API ab, um auf
  neue Versionen hinzuweisen. Dabei werden keine personenbezogenen Daten übertragen.

## Was die App NICHT tut

- Kein Benutzerkonto, kein Login.
- Keine Analyse-, Tracking- oder Telemetrie-Dienste.
- Keine Weitergabe von Daten an Dritte zu Werbezwecken.
- Keine Aufzeichnung von Gesprächen oder Nachrichten.

## Berechtigungen

- **Mikrofon**: für die Sprachübertragung. Du steuerst die Übertragung per
  Push-to-Talk-Taste.
- **Globale Tastatur-/Maus-Eingabe**: nur zum Erkennen der konfigurierten
  Push-to-Talk-Taste, auch während ein Spiel im Vordergrund läuft. Es werden keine
  Tastenanschläge protokolliert oder übertragen.

## Deine Rechte

Da die App keine personenbezogenen Daten dauerhaft speichert, gibt es keine
gespeicherten Profildaten, die gelöscht oder exportiert werden müssten. Bei Fragen:
<kontakt@raumdock.org>.

## Änderungen

Aktualisierungen dieser Erklärung werden an dieser Stelle veröffentlicht.

---

> **TODO vor Store-Submit:** Kontakt-Adresse prüfen, an stabiler URL veröffentlichen
> (z. B. `https://subraum.cc/privacy`) und ggf. englische Fassung
> hinzufügen. URL ins Partner-Center-Listing eintragen.
