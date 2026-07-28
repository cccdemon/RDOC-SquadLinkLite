//! Translations for the server-rendered pages (EN/DE/IT/ES/FR). Language is
//! chosen from `?lang=xx`, else the Accept-Language header, else English.

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Lang {
    En,
    De,
    It,
    Es,
    Fr,
}

pub const GITHUB_URL: &str = "https://github.com/cccdemon/RDOC-SquadLinkLite";
pub const RAUMDOCK_URL: &str = "https://raumdock.org";
/// Microsoft Store product page (Store ID 9N9NR49QFBF4) — the recommended,
/// code-signed install path. The direct/unsigned installer stays as a fallback.
///
/// Survives the rename: the listing was renamed in place to
/// "Subraum Communicator", so the Store ID and package identity are unchanged.
pub const STORE_URL: &str = "https://apps.microsoft.com/detail/9N9NR49QFBF4";
pub const FLEET_URL: &str = "https://suite.raumdock.org/fleetplanner";

impl Lang {
    pub fn code(self) -> &'static str {
        match self {
            Lang::En => "en",
            Lang::De => "de",
            Lang::It => "it",
            Lang::Es => "es",
            Lang::Fr => "fr",
        }
    }
    pub fn all() -> [Lang; 5] {
        [Lang::En, Lang::De, Lang::It, Lang::Es, Lang::Fr]
    }
    fn parse(code: &str) -> Option<Lang> {
        match code.trim().to_ascii_lowercase().get(0..2)? {
            "en" => Some(Lang::En),
            "de" => Some(Lang::De),
            "it" => Some(Lang::It),
            "es" => Some(Lang::Es),
            "fr" => Some(Lang::Fr),
            _ => None,
        }
    }
    /// `?lang=` wins; else the first matching Accept-Language tag; else English.
    pub fn detect(query: Option<&str>, accept: Option<&str>) -> Lang {
        if let Some(q) = query {
            if let Some(l) = Lang::parse(q) {
                return l;
            }
        }
        if let Some(a) = accept {
            for part in a.split(',') {
                let tag = part.split(';').next().unwrap_or("");
                if let Some(l) = Lang::parse(tag) {
                    return l;
                }
            }
        }
        Lang::En
    }
}

/// Screenshot gallery for the home page. The images ship inside the binary and
/// are served from `/assets/shot/<n>`; `count` says how many exist. Captions
/// describe the actual screenshots, so adding one means adding its caption here
/// in the same position.
pub fn screenshots(l: Lang, base: &str, count: usize) -> String {
    if count == 0 {
        return String::new();
    }
    let caps: [&str; 5] = match l {
        Lang::De => [
            "In der Session — Kanäle, Teilnehmer, Push-to-Talk, Chat",
            "Start — Session hosten oder mit Link und PIN beitreten",
            "Mehrere Funkkanäle, Wechsel per Hotkey",
            "Audio — Mikrofon, Push-to-Talk, App-Ducking, Funk-Klick",
            "Experte — Overlay, Kanal-Hotkeys, neu verschlüsseln, Selbsttest",
        ],
        Lang::It => [
            "In sessione — canali, partecipanti, push-to-talk, chat",
            "Avvio — ospita una sessione o entra con link e PIN",
            "Più canali radio, cambio con tasto rapido",
            "Audio — microfono, push-to-talk, abbassamento app, clic radio",
            "Esperto — overlay, tasti canale, nuova cifratura, autotest",
        ],
        Lang::Es => [
            "En sesión — canales, participantes, pulsar para hablar, chat",
            "Inicio — alojar una sesión o entrar con enlace y PIN",
            "Varios canales de radio, cambio por atajo",
            "Audio — micrófono, pulsar para hablar, atenuar apps, clic de radio",
            "Experto — superposición, atajos de canal, recifrar, autodiagnóstico",
        ],
        Lang::Fr => [
            "En session — canaux, participants, push-to-talk, chat",
            "Démarrage — héberger une session ou rejoindre avec lien et PIN",
            "Plusieurs canaux radio, changement par raccourci",
            "Audio — micro, push-to-talk, atténuation des apps, clic radio",
            "Expert — overlay, raccourcis de canal, rechiffrer, autotest",
        ],
        Lang::En => [
            "In a session — channels, roster, push-to-talk, chat",
            "Start — host a session or join with link and PIN",
            "Several radio channels, switched by hotkey",
            "Audio — mic, push-to-talk, app ducking, radio click",
            "Expert — overlay, channel hotkeys, re-encrypt, self-check",
        ],
    };
    let mut grid = String::new();
    for n in 1..=count.min(caps.len()) {
        let cap = caps[n - 1];
        grid.push_str(&format!(
            r#"<figure class="shot"><a href="{base}/assets/shot/{n}" target="_blank" rel="noopener"><img src="{base}/assets/shot/{n}" alt="{cap}" loading="lazy"></a><figcaption>{cap}</figcaption></figure>"#
        ));
    }
    format!(r#"<div class="shots">{grid}</div>"#)
}

/// Short social-share (OpenGraph) description, one line per language. No quotes
/// or markup — it is injected verbatim into an `og:description` attribute.
pub fn meta_desc(l: Lang) -> &'static str {
    match l {
        Lang::En => "Serverless P2P voice for small squads. Push-to-talk, no account, no recording — end-to-end and post-quantum encrypted.",
        Lang::De => "Serverloser P2P-Voicechat für kleine Crews. Push-to-Talk, ohne Account, ohne Aufnahme — Ende-zu-Ende und post-quanten-verschlüsselt.",
        Lang::It => "Voice chat P2P senza server per piccoli gruppi. Push-to-talk, senza account, senza registrazioni — cifrato end-to-end e post-quantistico.",
        Lang::Es => "Chat de voz P2P sin servidor para grupos pequeños. Pulsar para hablar, sin cuenta, sin grabación — cifrado de extremo a extremo y poscuántico.",
        Lang::Fr => "Chat vocal P2P sans serveur pour petites équipes. Push-to-talk, sans compte, sans enregistrement — chiffré de bout en bout et post-quantique.",
    }
}

/// Footer nav labels: [Download, Privacy, Legal, License].
pub fn nav(l: Lang) -> [&'static str; 4] {
    match l {
        Lang::En => ["Download", "Privacy", "Legal notice", "License"],
        Lang::De => ["Download", "Datenschutz", "Impressum", "Lizenz"],
        Lang::It => ["Download", "Privacy", "Note legali", "Licenza"],
        Lang::Es => ["Descargar", "Privacidad", "Aviso legal", "Licencia"],
        Lang::Fr => ["Télécharger", "Confidentialité", "Mentions légales", "Licence"],
    }
}

/// Language switcher: links to the same path with each `?lang=`.
pub fn switcher(path: &str, cur: Lang) -> String {
    let mut s = String::from("<nav class=\"lang\">");
    for l in Lang::all() {
        let on = if l == cur { " class=\"on\"" } else { "" };
        s.push_str(&format!(
            "<a href=\"{path}?lang={code}\"{on}>{label}</a>",
            code = l.code(),
            label = l.code().to_uppercase(),
        ));
    }
    s.push_str("</nav>");
    s
}

// ── Pages ────────────────────────────────────────────────────────────────────

/// Primary "Get it from Microsoft Store" button with an inline Microsoft logo
/// (inline SVG → no external image, works under the page CSP).
fn store_badge(label: &str) -> String {
    format!(
        r##"<a class="dl store" href="{STORE_URL}"><svg width="18" height="18" viewBox="0 0 24 24" aria-hidden="true"><rect x="1" y="1" width="10" height="10" fill="#f25022"/><rect x="13" y="1" width="10" height="10" fill="#7fba00"/><rect x="1" y="13" width="10" height="10" fill="#00a4ef"/><rect x="13" y="13" width="10" height="10" fill="#ffb900"/></svg>{label}</a>"##
    )
}

/// The topology schematic that carries the home page. It draws the one thing
/// that makes this product what it is: a dashed star for control (every peer
/// talks to the signaling service) laid over a solid mesh for data (every peer
/// talks to every other peer directly). The two link styles are the argument —
/// audio never touches the centre.
fn topology_svg(t: &HomeText) -> String {
    // Peers on a shallow arc; signaling box centred above them.
    let peers = [(80, 176), (268, 222), (452, 222), (640, 176)];
    let mut mesh = String::new();
    for i in 0..peers.len() {
        for j in (i + 1)..peers.len() {
            mesh.push_str(&format!(
                r#"<line x1="{}" y1="{}" x2="{}" y2="{}"/>"#,
                peers[i].0, peers[i].1, peers[j].0, peers[j].1
            ));
        }
    }
    let mut ctrl = String::new();
    let mut dots = String::new();
    for (x, y) in peers {
        ctrl.push_str(&format!(r#"<line x1="360" y1="74" x2="{x}" y2="{y}"/>"#));
        dots.push_str(&format!(r#"<circle cx="{x}" cy="{y}" r="9"/>"#));
    }
    format!(
        r##"<figure class="diagram">
<svg viewBox="0 0 720 250" role="img" aria-label="{alt}">
<title>{alt}</title>
<g stroke="#3D6FD8" stroke-width="2.5" stroke-linecap="round">{mesh}</g>
<g stroke="#E0A244" stroke-width="1.5" stroke-dasharray="3 7" stroke-linecap="round" opacity=".85">{ctrl}</g>
<rect x="252" y="14" width="216" height="60" fill="#141A24" stroke="#E0A244" stroke-width="1.5"/>
<text x="360" y="40" text-anchor="middle" fill="#E4E8EE" font-size="16" font-family="ui-monospace,monospace">{srv}</text>
<text x="360" y="60" text-anchor="middle" fill="#8C96A6" font-size="12" font-family="ui-monospace,monospace">{srv_sub}</text>
<g fill="#7FB0FF">{dots}</g>
</svg>
<figcaption>── {data} &nbsp;&nbsp; ┄┄ {ctrl_l}</figcaption>
</figure>"##,
        alt = t.dia_alt,
        srv = t.dia_server,
        srv_sub = t.dia_server_sub,
        data = t.dia_legend_data,
        ctrl_l = t.dia_legend_ctrl,
    )
}

/// Every string the home page needs, in one place per language. The layout below
/// exists exactly once, so a structural change cannot drift between languages.
struct HomeText {
    title: &'static str,
    eyebrow: &'static str,
    lede: &'static str,
    renamed: &'static str,
    dia_alt: &'static str,
    dia_server: &'static str,
    dia_server_sub: &'static str,
    dia_legend_data: &'static str,
    dia_legend_ctrl: &'static str,
    planes_eyebrow: &'static str,
    p2p_tag: &'static str,
    p2p_h: &'static str,
    p2p_b: &'static str,
    srv_tag: &'static str,
    srv_h: &'static str,
    srv_b: &'static str,
    spec_eyebrow: &'static str,
    spec_h: &'static str,
    spec_rows: [&'static str; 6],
    spec_seen: &'static str,
    spec_never: &'static str,
    steps_eyebrow: &'static str,
    steps_h: &'static str,
    steps: [&'static str; 3],
    get_eyebrow: &'static str,
    get_h: &'static str,
    store_btn: &'static str,
    store_note: &'static str,
    all_dl: &'static str,
    announce: &'static str,
    shots_eyebrow: &'static str,
    links_eyebrow: &'static str,
    l_fleet: &'static str,
    l_src: &'static str,
    l_legal: &'static str,
    l_priv: &'static str,
    l_lic: &'static str,
}

pub fn home(l: Lang, base: &str, shots: usize) -> (&'static str, String) {
    let t = home_text(l);
    let lc = l.code();
    // No screenshots on disk → no gallery section at all, rather than a heading
    // over six broken images.
    let shots_section = match screenshots(l, base, shots) {
        s if s.is_empty() => String::new(),
        s => format!(
            "<section class=\"sec\">\n<p class=\"eyebrow\">{}</p>\n{s}\n</section>\n",
            t.shots_eyebrow
        ),
    };
    // First three rows are what the signaling service must see to broker a
    // connection; the last three are what it structurally cannot see.
    let spec: String = t
        .spec_rows
        .iter()
        .enumerate()
        .map(|(i, label)| {
            let (cls, val) = if i < 3 { ("yes", t.spec_seen) } else { ("no", t.spec_never) };
            format!(r#"<div><span class="k">{label}</span><span class="v {cls}">{val}</span></div>"#)
        })
        .collect();
    let steps: String = t.steps.iter().map(|s| format!("<li>{s}</li>")).collect();

    let body = format!(
        r#"<section class="sec hero">
<p class="eyebrow">{eyebrow}</p>
<h1>subraum</h1>
<p class="tagline">encrypted communication</p>
<p class="lede">{lede}</p>
<p class="muted prose">{renamed}</p>
{diagram}
</section>

<section class="sec">
<p class="eyebrow">{planes_eyebrow}</p>
<div class="planes">
<div class="plane p2p"><span class="tag">{p2p_tag}</span><h3>{p2p_h}</h3><p>{p2p_b}</p></div>
<div class="plane srv"><span class="tag">{srv_tag}</span><h3>{srv_h}</h3><p>{srv_b}</p></div>
</div>
</section>

<section class="sec">
<p class="eyebrow">{spec_eyebrow}</p>
<h2>{spec_h}</h2>
<div class="spec prose">{spec}</div>
</section>

<section class="sec">
<p class="eyebrow">{steps_eyebrow}</p>
<h2>{steps_h}</h2>
<ol class="steps">{steps}</ol>
</section>

<section class="sec">
<p class="eyebrow">{get_eyebrow}</p>
<h2>{get_h}</h2>
<p>{store}</p>
<p class="muted prose">{store_note}</p>
<p><a class="dl" href="{base}/get">{all_dl}</a></p>
<p class="announce prose">{announce}</p>
</section>

{shots}
<section class="sec">
<p class="eyebrow">{links_eyebrow}</p>
<p class="links">
<a href="{RAUMDOCK_URL}">raumdock.org</a>
<a href="{FLEET_URL}">{l_fleet}</a>
<a href="{GITHUB_URL}">{l_src}</a>
</p>
<p class="muted"><a href="/privacy?lang={lc}">{l_priv}</a> · <a href="/legal?lang={lc}">{l_legal}</a> · <a href="/license?lang={lc}">{l_lic}</a></p>
{credits}
</section>"#,
        eyebrow = t.eyebrow,
        lede = t.lede,
        renamed = t.renamed,
        diagram = topology_svg(&t),
        planes_eyebrow = t.planes_eyebrow,
        p2p_tag = t.p2p_tag,
        p2p_h = t.p2p_h,
        p2p_b = t.p2p_b,
        srv_tag = t.srv_tag,
        srv_h = t.srv_h,
        srv_b = t.srv_b,
        spec_eyebrow = t.spec_eyebrow,
        spec_h = t.spec_h,
        spec = spec,
        steps_eyebrow = t.steps_eyebrow,
        steps_h = t.steps_h,
        steps = steps,
        get_eyebrow = t.get_eyebrow,
        get_h = t.get_h,
        store = store_badge(t.store_btn),
        store_note = t.store_note,
        all_dl = t.all_dl,
        announce = t.announce,
        shots = shots_section,
        links_eyebrow = t.links_eyebrow,
        l_fleet = t.l_fleet,
        l_src = t.l_src,
        l_priv = t.l_priv,
        l_legal = t.l_legal,
        l_lic = t.l_lic,
        credits = credits(l),
        base = base,
        lc = lc,
    );
    (t.title, body)
}

fn home_text(l: Lang) -> HomeText {
    match l {
        Lang::En => HomeText {
            title: "What is this?",
            eyebrow: "Serverless voice mesh · <b>warn-cap 12 · hard-cap 16</b>",
            lede: "Push-to-talk voice and chat for a small squad. Audio goes straight from player to player — no account, no recording, no media server in the middle.",
            renamed: "Previously released as SquadLink Lite. Renamed to subraum because an unrelated app already goes by \"SquadLink\"; the rename rules out the confusion. Same app, same team.",
            dia_alt: "Four peers connected directly to each other for voice, each with a separate dashed control link to the signaling service.",
            dia_server: "InitConnection",
            dia_server_sub: "signaling only",
            dia_legend_data: "voice + chat, peer to peer (Opus, DTLS-SRTP)",
            dia_legend_ctrl: "control: SDP, ICE, roster, PIN",
            planes_eyebrow: "Two planes",
            p2p_tag: "Data plane",
            p2p_h: "Between players",
            p2p_b: "Every peer holds a direct encrypted link to every other peer. Voice and chat travel only there. One shared room key seals group audio, so a frame is encoded and sealed once and fanned out.",
            srv_tag: "Control plane",
            srv_h: "One small service",
            srv_b: "InitConnection introduces peers to each other and keeps the roster and session PIN. It relays the handshake, then steps aside. It cannot decrypt what follows.",
            spec_eyebrow: "What crosses the centre",
            spec_h: "The signaling service sees exactly this",
            spec_rows: ["room name", "session PIN", "ICE candidates (IPs)", "voice audio", "chat messages", "room key"],
            spec_seen: "relayed",
            spec_never: "never leaves the peers",
            steps_eyebrow: "Getting a squad in",
            steps_h: "Three steps, no configuration",
            steps: [
                "The host creates a session in the app and gets a link and a 6-digit PIN.",
                "Mates open the link, install the app, enter code and PIN.",
                "The session stays alive while members are connected (max. 24&nbsp;hours).",
            ],
            get_eyebrow: "Install",
            get_h: "Get subraum",
            store_btn: "Get it from Microsoft Store",
            store_note: "The Microsoft Store version is signed and shows no warning. The direct installer is unsigned → Windows SmartScreen warns: \"More info\" then \"Run anyway\".",
            all_dl: "All downloads &amp; checksums →",
            announce: "Now on <strong>Windows, Linux (incl. SteamOS / Steam&nbsp;Deck), macOS and Android</strong>. <strong>iOS</strong> is finished and runs on-device — but App&nbsp;Store release is blocked on the Apple developer licence. <a href=\"https://twitch.tv/JustCallMeDeimos\">Support the stream</a> to make it happen.",
            shots_eyebrow: "The app",
            links_eyebrow: "Elsewhere",
            l_fleet: "RDOC Fleet Manager",
            l_src: "Source on GitHub",
            l_legal: "Legal notice",
            l_priv: "Privacy",
            l_lic: "License",
        },
        Lang::De => HomeText {
            title: "Was ist das?",
            eyebrow: "Serverloses Voice-Mesh · <b>Warn-Cap 12 · Hard-Cap 16</b>",
            lede: "Push-to-Talk-Sprache und Chat für eine kleine Crew. Der Ton läuft direkt von Spieler zu Spieler — ohne Account, ohne Aufnahme, ohne Medienserver dazwischen.",
            renamed: "Früher veröffentlicht als SquadLink Lite. Umbenannt in subraum, weil bereits eine andere App den Namen „SquadLink\" trägt; die Umbenennung schließt Verwechslungen aus. Gleiche App, gleiches Team.",
            dia_alt: "Vier Teilnehmer sind für Sprache direkt miteinander verbunden, jeder zusätzlich über eine gestrichelte Steuerverbindung mit dem Signaling-Dienst.",
            dia_server: "InitConnection",
            dia_server_sub: "nur Signaling",
            dia_legend_data: "Sprache + Chat, direkt zwischen Peers (Opus, DTLS-SRTP)",
            dia_legend_ctrl: "Steuerung: SDP, ICE, Teilnehmerliste, PIN",
            planes_eyebrow: "Zwei Ebenen",
            p2p_tag: "Datenebene",
            p2p_h: "Zwischen den Spielern",
            p2p_b: "Jeder Teilnehmer hält eine direkte verschlüsselte Verbindung zu jedem anderen. Sprache und Chat laufen ausschließlich dort. Ein gemeinsamer Room-Key sichert die Gruppensprache, ein Frame wird also einmal kodiert, einmal versiegelt und dann verteilt.",
            srv_tag: "Steuerebene",
            srv_h: "Ein kleiner Dienst",
            srv_b: "InitConnection stellt die Teilnehmer einander vor und verwaltet Liste und Session-PIN. Er vermittelt den Handshake und tritt dann zur Seite. Was danach läuft, kann er nicht entschlüsseln.",
            spec_eyebrow: "Was durch die Mitte geht",
            spec_h: "Genau das sieht der Signaling-Dienst",
            spec_rows: ["Room-Name", "Session-PIN", "ICE-Kandidaten (IPs)", "Sprache", "Chat-Nachrichten", "Room-Key"],
            spec_seen: "wird vermittelt",
            spec_never: "verlässt die Peers nie",
            steps_eyebrow: "Crew reinholen",
            steps_h: "Drei Schritte, keine Konfiguration",
            steps: [
                "Host erstellt in der App eine Session und erhält einen Link und eine 6-stellige PIN.",
                "Mitspieler öffnen den Link, installieren die App, geben Code und PIN ein.",
                "Die Session bleibt bestehen, solange Teilnehmer verbunden sind (maximal 24&nbsp;Stunden).",
            ],
            get_eyebrow: "Installieren",
            get_h: "subraum holen",
            store_btn: "Im Microsoft Store holen",
            store_note: "Die Microsoft-Store-Version ist signiert und warnt nicht. Der direkte Installer ist unsigniert → Windows SmartScreen warnt: „Weitere Informationen\" → „Trotzdem ausführen\".",
            all_dl: "Alle Downloads &amp; Prüfsummen →",
            announce: "Jetzt für <strong>Windows, Linux (inkl. SteamOS / Steam&nbsp;Deck), macOS und Android</strong>. <strong>iOS</strong> ist fertig und läuft auf dem Gerät — die Veröffentlichung im App&nbsp;Store scheitert aber noch an der Apple-Entwickler-Lizenz. <a href=\"https://twitch.tv/JustCallMeDeimos\">Unterstütze den Stream</a>, um das möglich zu machen.",
            shots_eyebrow: "Die App",
            links_eyebrow: "Weiterführend",
            l_fleet: "RDOC Fleetmanager",
            l_src: "Quellcode auf GitHub",
            l_legal: "Impressum",
            l_priv: "Datenschutz",
            l_lic: "Lizenz",
        },
        Lang::It => HomeText {
            title: "Che cos'è?",
            eyebrow: "Voice mesh senza server · <b>soglia 12 · limite 16</b>",
            lede: "Voce push-to-talk e chat per un piccolo gruppo. L'audio passa direttamente da giocatore a giocatore — senza account, senza registrazioni, senza media server nel mezzo.",
            renamed: "Pubblicata in precedenza come SquadLink Lite. Rinominata in subraum perché esiste già un'altra app chiamata \"SquadLink\"; il cambio di nome evita ogni confusione. Stessa app, stesso team.",
            dia_alt: "Quattro partecipanti collegati direttamente tra loro per la voce, ciascuno con un collegamento di controllo tratteggiato verso il servizio di signaling.",
            dia_server: "InitConnection",
            dia_server_sub: "solo signaling",
            dia_legend_data: "voce + chat, da peer a peer (Opus, DTLS-SRTP)",
            dia_legend_ctrl: "controllo: SDP, ICE, elenco, PIN",
            planes_eyebrow: "Due piani",
            p2p_tag: "Piano dati",
            p2p_h: "Tra i giocatori",
            p2p_b: "Ogni partecipante mantiene un collegamento cifrato diretto con ogni altro. Voce e chat viaggiano solo lì. Una chiave di stanza condivisa protegge l'audio di gruppo: un frame viene codificato e sigillato una volta sola e poi distribuito.",
            srv_tag: "Piano di controllo",
            srv_h: "Un piccolo servizio",
            srv_b: "InitConnection presenta i partecipanti tra loro e gestisce l'elenco e il PIN di sessione. Media l'handshake e poi si fa da parte. Non può decifrare ciò che segue.",
            spec_eyebrow: "Cosa passa dal centro",
            spec_h: "Il servizio di signaling vede esattamente questo",
            spec_rows: ["nome della stanza", "PIN di sessione", "candidati ICE (IP)", "audio della voce", "messaggi di chat", "chiave di stanza"],
            spec_seen: "viene mediato",
            spec_never: "non lascia mai i peer",
            steps_eyebrow: "Far entrare il gruppo",
            steps_h: "Tre passaggi, nessuna configurazione",
            steps: [
                "L'host crea una sessione nell'app e riceve un link e un PIN di 6 cifre.",
                "I compagni aprono il link, installano l'app, inseriscono codice e PIN.",
                "La sessione resta attiva finché ci sono partecipanti collegati (max 24&nbsp;ore).",
            ],
            get_eyebrow: "Installazione",
            get_h: "Ottieni subraum",
            store_btn: "Scarica dal Microsoft Store",
            store_note: "La versione del Microsoft Store è firmata e non mostra avvisi. L'installer diretto non è firmato → Windows SmartScreen avvisa: \"Ulteriori informazioni\" → \"Esegui comunque\".",
            all_dl: "Tutti i download e i checksum →",
            announce: "Ora su <strong>Windows, Linux (incl. SteamOS / Steam&nbsp;Deck), macOS e Android</strong>. <strong>iOS</strong> è pronto e gira sul dispositivo — ma la pubblicazione sull'App&nbsp;Store è bloccata dalla licenza sviluppatore Apple. <a href=\"https://twitch.tv/JustCallMeDeimos\">Sostieni lo stream</a> per renderlo possibile.",
            shots_eyebrow: "L'app",
            links_eyebrow: "Altrove",
            l_fleet: "RDOC Fleet Manager",
            l_src: "Codice su GitHub",
            l_legal: "Note legali",
            l_priv: "Privacy",
            l_lic: "Licenza",
        },
        Lang::Es => HomeText {
            title: "¿Qué es esto?",
            eyebrow: "Malla de voz sin servidor · <b>aviso 12 · límite 16</b>",
            lede: "Voz por pulsar para hablar y chat para un grupo pequeño. El audio va directo de jugador a jugador — sin cuenta, sin grabación, sin servidor de medios en medio.",
            renamed: "Publicada anteriormente como SquadLink Lite. Renombrada a subraum porque ya existe otra aplicación llamada \"SquadLink\"; el cambio de nombre evita la confusión. La misma app, el mismo equipo.",
            dia_alt: "Cuatro participantes conectados directamente entre sí para la voz, cada uno con un enlace de control discontinuo hacia el servicio de señalización.",
            dia_server: "InitConnection",
            dia_server_sub: "solo señalización",
            dia_legend_data: "voz + chat, entre pares (Opus, DTLS-SRTP)",
            dia_legend_ctrl: "control: SDP, ICE, lista, PIN",
            planes_eyebrow: "Dos planos",
            p2p_tag: "Plano de datos",
            p2p_h: "Entre los jugadores",
            p2p_b: "Cada participante mantiene un enlace cifrado directo con todos los demás. La voz y el chat solo circulan ahí. Una clave de sala compartida protege el audio del grupo: cada trama se codifica y se sella una sola vez y luego se reparte.",
            srv_tag: "Plano de control",
            srv_h: "Un servicio pequeño",
            srv_b: "InitConnection presenta a los participantes entre sí y guarda la lista y el PIN de sesión. Media el saludo inicial y luego se aparta. No puede descifrar lo que sigue.",
            spec_eyebrow: "Qué pasa por el centro",
            spec_h: "El servicio de señalización ve exactamente esto",
            spec_rows: ["nombre de la sala", "PIN de sesión", "candidatos ICE (IP)", "audio de voz", "mensajes de chat", "clave de sala"],
            spec_seen: "se media",
            spec_never: "nunca sale de los pares",
            steps_eyebrow: "Meter al grupo",
            steps_h: "Tres pasos, sin configuración",
            steps: [
                "El anfitrión crea una sesión en la app y obtiene un enlace y un PIN de 6 dígitos.",
                "Los compañeros abren el enlace, instalan la app e introducen código y PIN.",
                "La sesión sigue activa mientras haya participantes conectados (máx. 24&nbsp;horas).",
            ],
            get_eyebrow: "Instalación",
            get_h: "Consigue subraum",
            store_btn: "Descargar de Microsoft Store",
            store_note: "La versión de Microsoft Store está firmada y no muestra avisos. El instalador directo no está firmado → Windows SmartScreen avisa: \"Más información\" → \"Ejecutar de todas formas\".",
            all_dl: "Todas las descargas y sumas de verificación →",
            announce: "Ya en <strong>Windows, Linux (incl. SteamOS / Steam&nbsp;Deck), macOS y Android</strong>. <strong>iOS</strong> está terminado y funciona en el dispositivo — pero la publicación en la App&nbsp;Store depende de la licencia de desarrollador de Apple. <a href=\"https://twitch.tv/JustCallMeDeimos\">Apoya el stream</a> para hacerlo posible.",
            shots_eyebrow: "La app",
            links_eyebrow: "En otros sitios",
            l_fleet: "RDOC Fleet Manager",
            l_src: "Código en GitHub",
            l_legal: "Aviso legal",
            l_priv: "Privacidad",
            l_lic: "Licencia",
        },
        Lang::Fr => HomeText {
            title: "Qu'est-ce que c'est ?",
            eyebrow: "Maillage vocal sans serveur · <b>alerte 12 · limite 16</b>",
            lede: "Voix en push-to-talk et chat pour une petite équipe. L'audio va directement d'un joueur à l'autre — sans compte, sans enregistrement, sans serveur média au milieu.",
            renamed: "Publiée auparavant sous le nom SquadLink Lite. Renommée subraum car une autre application porte déjà le nom « SquadLink » ; ce changement évite toute confusion. Même app, même équipe.",
            dia_alt: "Quatre participants reliés directement entre eux pour la voix, chacun avec une liaison de contrôle en pointillés vers le service de signalisation.",
            dia_server: "InitConnection",
            dia_server_sub: "signalisation seule",
            dia_legend_data: "voix + chat, de pair à pair (Opus, DTLS-SRTP)",
            dia_legend_ctrl: "contrôle : SDP, ICE, liste, PIN",
            planes_eyebrow: "Deux plans",
            p2p_tag: "Plan de données",
            p2p_h: "Entre les joueurs",
            p2p_b: "Chaque participant garde une liaison chiffrée directe avec tous les autres. La voix et le chat n'y circulent que là. Une clé de salon partagée protège l'audio de groupe : une trame est encodée et scellée une seule fois, puis diffusée.",
            srv_tag: "Plan de contrôle",
            srv_h: "Un petit service",
            srv_b: "InitConnection présente les participants entre eux et tient la liste et le code PIN de session. Il relaie la poignée de main puis s'efface. Il ne peut pas déchiffrer ce qui suit.",
            spec_eyebrow: "Ce qui passe par le centre",
            spec_h: "Le service de signalisation voit exactement ceci",
            spec_rows: ["nom du salon", "PIN de session", "candidats ICE (IP)", "audio de la voix", "messages de chat", "clé de salon"],
            spec_seen: "relayé",
            spec_never: "ne quitte jamais les pairs",
            steps_eyebrow: "Faire entrer l'équipe",
            steps_h: "Trois étapes, aucune configuration",
            steps: [
                "L'hôte crée une session dans l'app et obtient un lien et un code PIN à 6 chiffres.",
                "Les coéquipiers ouvrent le lien, installent l'app, saisissent le code et le PIN.",
                "La session reste active tant que des participants sont connectés (max. 24&nbsp;heures).",
            ],
            get_eyebrow: "Installation",
            get_h: "Obtenir subraum",
            store_btn: "Obtenir sur le Microsoft Store",
            store_note: "La version du Microsoft Store est signée et n'affiche aucun avertissement. L'installeur direct n'est pas signé → Windows SmartScreen avertit : « Informations complémentaires » → « Exécuter quand même ».",
            all_dl: "Tous les téléchargements et sommes de contrôle →",
            announce: "Désormais sur <strong>Windows, Linux (dont SteamOS / Steam&nbsp;Deck), macOS et Android</strong>. <strong>iOS</strong> est prêt et tourne sur l'appareil — mais la publication sur l'App&nbsp;Store est bloquée par la licence développeur Apple. <a href=\"https://twitch.tv/JustCallMeDeimos\">Soutiens le stream</a> pour rendre ça possible.",
            shots_eyebrow: "L'application",
            links_eyebrow: "Ailleurs",
            l_fleet: "RDOC Fleet Manager",
            l_src: "Code sur GitHub",
            l_legal: "Mentions légales",
            l_priv: "Confidentialité",
            l_lic: "Licence",
        },
    }
}

/// Tester + author credits, appended to the home page.
fn credits(l: Lang) -> String {
    let (tested, concept, ai) = match l {
        Lang::En => ("Tested by", "Concept &amp; programming by", "Yes, AI was involved."),
        Lang::De => ("Getestet von", "Konzept &amp; Programmierung von", "Ja, KI war beteiligt."),
        Lang::It => ("Testato da", "Ideazione e programmazione di", "Sì, è stata coinvolta l'IA."),
        Lang::Es => ("Probado por", "Concepto y programación de", "Sí, hubo IA involucrada."),
        Lang::Fr => ("Testé par", "Conception et programmation par", "Oui, l'IA a participé."),
    };
    format!(
        r#"<p class="eyebrow" style="margin-top:2rem">{tested}</p>
<p class="links">
<a href="https://twitch.tv/smorxel">twitch.tv/smorxel</a>
<a href="https://twitch.tv/JerichoRamirez">twitch.tv/JerichoRamirez</a>
<a href="https://twitch.tv/JustCallMeDeimos">twitch.tv/JustCallMeDeimos</a>
<a href="https://twitch.tv/stormp00per89">twitch.tv/stormp00per89</a>
<a href="https://twitch.tv/x_jazzz_x">twitch.tv/x_jazzz_x</a>
<span>head87x</span>
</p>
<p class="muted">{concept} JustCallMeDeimos &amp; xhead87x (Claude Code &amp; Codex for crosstesting). {ai}</p>"#
    )
}

// ── Download page (/get) ──────────────────────────────────────────────────────

/// One downloadable artifact, parsed from the mirror's manifest.json.
pub struct Artifact {
    pub platform: String, // "windows" | "linux" | "android"
    pub arch: String,     // "x64" | "amd64" | "arm64" | "armv7" | "x86_64" | "universal"
    pub file: String,
    pub size: u64,
    pub sha256: String,
}

/// Minimal HTML escape for values interpolated into the page (defense in depth;
/// manifest values are CI-controlled but never trust blindly).
fn esc(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

/// Human-readable byte size, e.g. 10973184 → "10.5 MB".
fn human_size(bytes: u64) -> String {
    let mb = bytes as f64 / 1_048_576.0;
    if mb >= 1.0 {
        format!("{mb:.1} MB")
    } else {
        format!("{:.0} KB", bytes as f64 / 1024.0)
    }
}

struct DlText {
    title: &'static str,
    intro: &'static str,
    store_btn: &'static str,
    store_note: &'static str,
    win: &'static str,
    win_note: &'static str,
    linux: &'static str,
    linux_note: &'static str,
    android: &'static str,
    android_note: &'static str,
    verify: &'static str,
    none: &'static str,
}

fn dl_text(l: Lang) -> DlText {
    match l {
        Lang::En => DlText {
            title: "Downloads",
            intro: "Recommended: the Microsoft Store build is code-signed and installs without any warning. The direct downloads below are unsigned — verify the SHA-256 after downloading.",
            store_btn: "Get it from Microsoft Store",
            store_note: "The Store version updates automatically and shows no SmartScreen warning.",
            win: "Windows",
            win_note: "Unsigned — Windows SmartScreen warns: \"More info\" then \"Run anyway\".",
            linux: "Linux",
            linux_note: "Unsigned .deb / .rpm / .AppImage. Make AppImages executable: chmod +x.",
            android: "Android",
            android_note: "Debug-signed APK for sideloading (testing). Enable \"install unknown apps\".",
            verify: "SHA-256:",
            none: "No builds available yet — check back after the next release.",
        },
        Lang::De => DlText {
            title: "Downloads",
            intro: "Empfohlen: Die Microsoft-Store-Version ist signiert und installiert ohne Warnung. Die direkten Downloads unten sind unsigniert — prüfe nach dem Download die SHA-256.",
            store_btn: "Im Microsoft Store holen",
            store_note: "Die Store-Version aktualisiert sich automatisch und zeigt keine SmartScreen-Warnung.",
            win: "Windows",
            win_note: "Unsigniert — Windows SmartScreen warnt: „Weitere Informationen\" → „Trotzdem ausführen\".",
            linux: "Linux",
            linux_note: "Unsigniertes .deb / .rpm / .AppImage. AppImage ausführbar machen: chmod +x.",
            android: "Android",
            android_note: "Debug-signierte APK zum Sideloaden (Test). „Unbekannte Apps installieren\" erlauben.",
            verify: "SHA-256:",
            none: "Noch keine Builds verfügbar — schau nach dem nächsten Release wieder vorbei.",
        },
        Lang::It => DlText {
            title: "Download",
            intro: "Consigliato: la versione del Microsoft Store è firmata e si installa senza avvisi. I download diretti qui sotto non sono firmati — verifica lo SHA-256 dopo il download.",
            store_btn: "Scarica dal Microsoft Store",
            store_note: "La versione dello Store si aggiorna da sola e non mostra avvisi SmartScreen.",
            win: "Windows",
            win_note: "Non firmato — Windows SmartScreen avvisa: \"Ulteriori informazioni\" → \"Esegui comunque\".",
            linux: "Linux",
            linux_note: ".deb / .rpm / .AppImage non firmati. Rendi eseguibile l'AppImage: chmod +x.",
            android: "Android",
            android_note: "APK con firma di debug per il sideload (test). Abilita \"installa app sconosciute\".",
            verify: "SHA-256:",
            none: "Nessuna build disponibile — torna dopo la prossima release.",
        },
        Lang::Es => DlText {
            title: "Descargas",
            intro: "Recomendado: la versión de Microsoft Store está firmada y se instala sin avisos. Las descargas directas de abajo no están firmadas — verifica el SHA-256 tras descargar.",
            store_btn: "Descargar de Microsoft Store",
            store_note: "La versión de la Store se actualiza sola y no muestra avisos de SmartScreen.",
            win: "Windows",
            win_note: "Sin firmar — Windows SmartScreen avisa: \"Más información\" → \"Ejecutar de todas formas\".",
            linux: "Linux",
            linux_note: ".deb / .rpm / .AppImage sin firmar. Haz ejecutable el AppImage: chmod +x.",
            android: "Android",
            android_note: "APK firmada en depuración para instalación manual (pruebas). Activa \"instalar apps desconocidas\".",
            verify: "SHA-256:",
            none: "Aún no hay compilaciones — vuelve tras la próxima versión.",
        },
        Lang::Fr => DlText {
            title: "Téléchargements",
            intro: "Recommandé : la version du Microsoft Store est signée et s'installe sans avertissement. Les téléchargements directs ci-dessous ne sont pas signés — vérifiez le SHA-256 après téléchargement.",
            store_btn: "Obtenir sur le Microsoft Store",
            store_note: "La version du Store se met à jour automatiquement et n'affiche aucun avertissement SmartScreen.",
            win: "Windows",
            win_note: "Non signé — Windows SmartScreen avertit : « Informations complémentaires » → « Exécuter quand même ».",
            linux: "Linux",
            linux_note: ".deb / .rpm / .AppImage non signés. Rendez l'AppImage exécutable : chmod +x.",
            android: "Android",
            android_note: "APK signé en debug pour le sideload (test). Activez « installer des applis inconnues ».",
            verify: "SHA-256 :",
            none: "Aucune version disponible pour l'instant — revenez après la prochaine release.",
        },
    }
}

/// One platform section: heading + note + a list of artifact rows. Empty string
/// when no artifact matches `platform`, so the section is omitted entirely.
fn dl_section(base: &str, head: &str, note: &str, verify: &str, platform: &str, arts: &[Artifact]) -> String {
    let rows: String = arts
        .iter()
        .filter(|a| a.platform == platform)
        .map(|a| {
            format!(
                r#"<li><a class="file" href="{base}/download/{file}">{file}</a>
<span class="meta">{size} · {arch}</span>
<span class="sha">{verify} {sha}</span></li>"#,
                base = base,
                file = esc(&a.file),
                size = human_size(a.size),
                arch = esc(&a.arch),
                verify = verify,
                sha = esc(&a.sha256),
            )
        })
        .collect();
    if rows.is_empty() {
        String::new()
    } else {
        format!(
            "<section class=\"sec\">\n<p class=\"eyebrow\">{head}</p>\n<p class=\"muted prose\">{note}</p>\n<ul class=\"arts\">{rows}</ul>\n</section>\n"
        )
    }
}

/// Localized download page: MS Store badge + per-platform artifact lists with
/// SHA-256, fed by the mirror's manifest.json (`arts`). `version` is shown in
/// the heading when known.
pub fn downloads(l: Lang, base: &str, version: Option<&str>, arts: &[Artifact]) -> (&'static str, String) {
    let t = dl_text(l);
    let ver = version.map(|v| format!("v{}", esc(v))).unwrap_or_else(|| "—".into());
    let mut body = format!(
        // Platform names and "SHA-256" are proper nouns, so the eyebrow needs no
        // translation — only the version varies.
        r#"<section class="sec hero">
<p class="eyebrow">Windows · Linux · macOS · Android · SHA-256 · <b>{ver}</b></p>
<h1>{title}</h1>
<p class="lede">{intro}</p>
<p>{store}</p>
<p class="muted prose">{store_note}</p>
</section>
"#,
        ver = ver,
        title = t.title,
        intro = t.intro,
        store = store_badge(t.store_btn),
        store_note = t.store_note,
    );
    if arts.is_empty() {
        body.push_str(&format!("<section class=\"sec\"><p class=\"muted\">{}</p></section>", t.none));
        return (t.title, body);
    }
    body.push_str(&dl_section(base, t.win, t.win_note, t.verify, "windows", arts));
    body.push_str(&dl_section(base, t.linux, t.linux_note, t.verify, "linux", arts));
    body.push_str(&dl_section(base, t.android, t.android_note, t.verify, "android", arts));
    (t.title, body)
}

/// Wraps a static document body in the page's section/measure structure. Keeping
/// this here means the legal texts stay plain HTML in one language each — the
/// layout is applied once instead of being duplicated into fifteen constants.
pub fn doc(body: &str) -> String {
    format!("<section class=\"sec hero\"><div class=\"prose\">{body}</div></section>")
}

pub fn privacy(l: Lang) -> (&'static str, String) {
    match l {
        Lang::En => ("Privacy", doc(PRIVACY_EN)),
        Lang::De => ("Datenschutz", doc(PRIVACY_DE)),
        Lang::It => ("Privacy", doc(PRIVACY_IT)),
        Lang::Es => ("Privacidad", doc(PRIVACY_ES)),
        Lang::Fr => ("Confidentialité", doc(PRIVACY_FR)),
    }
}

pub fn legal(l: Lang) -> (&'static str, String) {
    match l {
        Lang::En => ("Legal notice", doc(LEGAL_EN)),
        Lang::De => ("Impressum", doc(LEGAL_DE)),
        Lang::It => ("Note legali", doc(LEGAL_IT)),
        Lang::Es => ("Aviso legal", doc(LEGAL_ES)),
        Lang::Fr => ("Mentions légales", doc(LEGAL_FR)),
    }
}

pub fn license(l: Lang) -> (&'static str, String) {
    let body = |intro: &str, h: &str, b1: &str, b2: &str, b3: &str, b4: &str, ch: &str, ct: &str, mail: &str, sumh: &str, full: &str, foot: &str| {
        format!(
            r#"<h1>{intro}</h1>
<p>subraum — <b>PolyForm Noncommercial License 1.0.0</b>.</p>
<h2>{h}</h2>
<ul><li>{b1}</li><li>{b2}</li><li>{b3}</li><li>{b4}</li></ul>
<h2>{ch}</h2>
<p>{ct}</p>
<p>{mail}: <a href="mailto:commercialusage@raumdock.org">commercialusage@raumdock.org</a></p>
<p>{sumh}</p>
<p><a class="dl" href="{GITHUB_URL}/blob/main/LICENSE">{full}</a></p>
<p class="muted">© head87x &amp; justcallmedeimos. {foot}</p>"#
        )
    };
    let body = |a, b, c, d, e, f, g, h, i, j, k, m| doc(&body(a, b, c, d, e, f, g, h, i, j, k, m));
    match l {
        Lang::En => ("License", body(
            "License — non-commercial", "In short",
            "Use, copy, modify, share — for any non-commercial purpose (private, community, education, research).",
            "No commercial use without a separate license.",
            "Keep the license and copyright notices.",
            "Provided as is, without warranty or liability.",
            "Commercial use", "Commercial use requires a separate commercial license: selling, sublicensing, hosting as a paid service, integrating into commercial products, or use in revenue-generating activities.",
            "Inquiries", "This is a summary — the full license text is binding:", "View the full license (LICENSE)", "PolyForm Noncommercial License 1.0.0 — see polyformproject.org.")),
        Lang::De => ("Lizenz", body(
            "Lizenz — nicht-kommerziell", "Kurz gesagt",
            "Nutzen, kopieren, ändern, weitergeben — für jeden nicht-kommerziellen Zweck (privat, Community, Bildung, Forschung).",
            "Keine kommerzielle Nutzung ohne gesonderte Lizenz.",
            "Lizenz- und Urhebervermerke beibehalten.",
            "Ohne Gewähr / ohne Haftung.",
            "Kommerzielle Nutzung", "Kommerzielle Nutzung erfordert eine separate kommerzielle Lizenz: Verkauf, Unterlizenzierung, Betrieb als bezahlter Dienst, Integration in kommerzielle Produkte oder Nutzung in umsatzgenerierenden Aktivitäten.",
            "Anfragen", "Dies ist eine Zusammenfassung — verbindlich ist der vollständige Lizenztext:", "Vollständige Lizenz (LICENSE) ansehen", "PolyForm Noncommercial License 1.0.0 — siehe polyformproject.org.")),
        Lang::It => ("Licenza", body(
            "Licenza — non commerciale", "In breve",
            "Usare, copiare, modificare, condividere — per qualsiasi scopo non commerciale (privato, community, istruzione, ricerca).",
            "Nessun uso commerciale senza una licenza separata.",
            "Mantenere gli avvisi di licenza e copyright.",
            "Fornito così com'è, senza garanzie né responsabilità.",
            "Uso commerciale", "L'uso commerciale richiede una licenza commerciale separata: vendita, sublicenza, hosting come servizio a pagamento, integrazione in prodotti commerciali o uso in attività che generano ricavi.",
            "Richieste", "Questo è un riassunto — fa fede il testo completo della licenza:", "Vedi la licenza completa (LICENSE)", "PolyForm Noncommercial License 1.0.0 — vedi polyformproject.org.")),
        Lang::Es => ("Licencia", body(
            "Licencia — no comercial", "En resumen",
            "Usar, copiar, modificar, compartir — para cualquier fin no comercial (privado, comunidad, educación, investigación).",
            "Sin uso comercial sin una licencia aparte.",
            "Conservar los avisos de licencia y copyright.",
            "Se ofrece tal cual, sin garantía ni responsabilidad.",
            "Uso comercial", "El uso comercial requiere una licencia comercial aparte: venta, sublicencia, alojamiento como servicio de pago, integración en productos comerciales o uso en actividades que generan ingresos.",
            "Consultas", "Esto es un resumen — el texto completo de la licencia es vinculante:", "Ver la licencia completa (LICENSE)", "PolyForm Noncommercial License 1.0.0 — ver polyformproject.org.")),
        Lang::Fr => ("Licence", body(
            "Licence — non commerciale", "En bref",
            "Utiliser, copier, modifier, partager — pour tout usage non commercial (privé, communauté, éducation, recherche).",
            "Aucun usage commercial sans licence distincte.",
            "Conserver les mentions de licence et de droit d'auteur.",
            "Fourni en l'état, sans garantie ni responsabilité.",
            "Usage commercial", "L'usage commercial nécessite une licence commerciale distincte : vente, sous-licence, hébergement en service payant, intégration dans des produits commerciaux ou usage dans des activités génératrices de revenus.",
            "Demandes", "Ceci est un résumé — le texte complet de la licence fait foi :", "Voir la licence complète (LICENSE)", "PolyForm Noncommercial License 1.0.0 — voir polyformproject.org.")),
    }
}

/// Share-link landing in the chosen language (`code` is already HTML-escaped).
pub fn landing(l: Lang, base: &str, code: &str) -> String {
    // (intro, codelbl, step_install, step2, store_btn, unsigned_btn, ss_note, foot)
    let (intro, codelbl, step1, step2, store_btn, unsigned_btn, ss_note, foot) = match l {
        Lang::En => ("You have been invited to a voice session.", "Session code:", "Install the app — Microsoft Store recommended.", "Open the app → Join → enter the code + the 6-digit PIN (from the host).", "Get it from Microsoft Store", "Direct download (unsigned installer)", "The direct installer is not code-signed, so Windows SmartScreen shows a warning. To install anyway: click \u{201c}More info\u{201d} \u{2192} \u{201c}Run anyway\u{201d}. The Microsoft Store version shows no warning.", "Audio runs directly peer-to-peer (encrypted). The server only brokers."),
        Lang::De => ("Du wurdest zu einer Voice-Session eingeladen.", "Session-Code:", "App installieren \u{2014} Microsoft Store empfohlen.", "App \u{f6}ffnen \u{2192} Beitreten \u{2192} Code + die 6-stellige PIN (vom Host) eingeben.", "Im Microsoft Store holen", "Direkter Download (unsigniertes Installationsprogramm)", "Der direkte Installer ist nicht signiert, daher warnt Windows SmartScreen. Trotzdem installieren: auf \u{201e}Weitere Informationen\u{201c} \u{2192} \u{201e}Trotzdem ausf\u{fc}hren\u{201c} klicken. Die Microsoft-Store-Version warnt nicht.", "Audio l\u{e4}uft direkt Peer-zu-Peer (verschl\u{fc}sselt). Der Server vermittelt nur."),
        Lang::It => ("Sei stato invitato a una sessione vocale.", "Codice sessione:", "Installa l'app \u{2014} Microsoft Store consigliato.", "Apri l'app \u{2192} Partecipa \u{2192} inserisci il codice + il PIN di 6 cifre (dall'host).", "Scarica dal Microsoft Store", "Download diretto (installer non firmato)", "L'installer diretto non \u{e8} firmato, quindi Windows SmartScreen mostra un avviso. Per installare comunque: \u{201c}Ulteriori informazioni\u{201d} \u{2192} \u{201c}Esegui comunque\u{201d}. La versione del Microsoft Store non mostra avvisi.", "L'audio \u{e8} diretto peer-to-peer (cifrato). Il server fa solo da tramite."),
        Lang::Es => ("Te han invitado a una sesi\u{f3}n de voz.", "C\u{f3}digo de sesi\u{f3}n:", "Instala la app \u{2014} Microsoft Store recomendado.", "Abre la app \u{2192} Unirse \u{2192} introduce el c\u{f3}digo + el PIN de 6 d\u{ed}gitos (del anfitri\u{f3}n).", "Descargar de Microsoft Store", "Descarga directa (instalador sin firmar)", "El instalador directo no est\u{e1} firmado, por lo que Windows SmartScreen muestra una advertencia. Para instalar igualmente: \u{201c}M\u{e1}s informaci\u{f3}n\u{201d} \u{2192} \u{201c}Ejecutar de todas formas\u{201d}. La versi\u{f3}n de Microsoft Store no muestra advertencias.", "El audio es directo peer-to-peer (cifrado). El servidor solo intermedia."),
        Lang::Fr => ("Vous avez \u{e9}t\u{e9} invit\u{e9} \u{e0} une session vocale.", "Code de session :", "Installez l'app \u{2014} Microsoft Store recommand\u{e9}.", "Ouvrez l'app \u{2192} Rejoindre \u{2192} saisissez le code + le PIN \u{e0} 6 chiffres (de l'h\u{f4}te).", "Obtenir sur le Microsoft Store", "T\u{e9}l\u{e9}chargement direct (installeur non sign\u{e9})", "L'installeur direct n'est pas sign\u{e9}, donc Windows SmartScreen affiche un avertissement. Pour installer quand m\u{ea}me : \u{ab} Informations compl\u{e9}mentaires \u{bb} \u{2192} \u{ab} Ex\u{e9}cuter quand m\u{ea}me \u{bb}. La version du Microsoft Store n'affiche aucun avertissement.", "L'audio est direct pair-\u{e0}-pair (chiffr\u{e9}). Le serveur ne fait que l'interm\u{e9}diaire."),
    };
    // The code is the reason the visitor is here, so it leads — headline first,
    // then the two steps that turn it into a working session.
    format!(
        r#"<section class="sec hero">
<p class="eyebrow">{codelbl}</p>
<p class="code">{code}</p>
<h1>subraum</h1>
<p class="tagline">encrypted communication</p>
<p class="lede">{intro}</p>
</section>

<section class="sec">
<ol class="steps">
<li>{step1}</li>
<li>{step2}</li>
</ol>
<p><a class="dl store" href="{STORE_URL}">{store_btn}</a></p>
<p><a class="dl" href="{base}/download/">{unsigned_btn}</a></p>
<p class="muted prose">{ss_note}</p>
<p class="muted prose">{foot}</p>
</section>"#
    )
}

// ── Long page bodies kept as constants for readability ───────────────────────

const PRIVACY_EN: &str = r#"<h1>Privacy</h1>
<p class="muted">subraum is built for data minimisation.</p>
<h2>What does NOT happen</h2>
<ul>
<li><b>No audio/chat recording.</b> Voice and text are peer-to-peer (DTLS-SRTP / encrypted DataChannel) and are stored nowhere.</li>
<li><b>No accounts</b>, no login, no tracking, no ads, no cookies.</li>
<li>The brokering server <b>never sees media</b> — voice/chat never pass through it.</li>
</ul>
<h2>What is processed</h2>
<ul>
<li><b>Signaling</b>: to connect, the apps exchange connection data via the server (SDP/ICE candidates, chosen display name, room/session mapping). This lives only <b>in memory</b> and is dropped once the room is empty (within 24&nbsp;h at most).</li>
<li><b>Session brokering</b>: a random code + 6-digit PIN are held temporarily in memory (max 24&nbsp;h) to let mates join without configuration.</li>
<li><b>Connection metadata</b>: like any internet service the server technically sees IP addresses on connect; they are not persistently logged.</li>
<li><b>TURN relay (fallback only)</b>: if no direct path is possible, encrypted audio may pass through a relay. It forwards only <b>encrypted bytes</b> and cannot decrypt them.</li>
</ul>
<h2>Third parties</h2>
<p>Installers are served via GitHub Releases (GitHub's privacy terms apply to the download). STUN/TURN may use public STUN servers for NAT discovery.</p>
<h2>Contact</h2>
<p>Controller: see the <a href="/legal?lang=en">legal notice</a>. Requests via <a href="https://raumdock.org">raumdock.org</a>.</p>"#;

const PRIVACY_DE: &str = r#"<h1>Datenschutzerklärung</h1>
<p class="muted">subraum ist auf Datensparsamkeit ausgelegt.</p>
<h2>Was NICHT passiert</h2>
<ul>
<li><b>Keine Audio-/Chat-Aufzeichnung.</b> Sprache und Text laufen Peer-to-Peer (DTLS-SRTP bzw. verschlüsselter DataChannel) und werden nirgends gespeichert.</li>
<li><b>Keine Benutzerkonten</b>, kein Login, kein Tracking, keine Werbung, keine Cookies.</li>
<li>Der Vermittlungsserver <b>sieht den Medieninhalt nicht</b> — Stimme/Chat fließen nie über ihn.</li>
</ul>
<h2>Was verarbeitet wird</h2>
<ul>
<li><b>Signaling</b>: Beim Verbinden tauschen die Apps über den Server Verbindungsdaten aus (SDP/ICE-Kandidaten, Anzeigename, Raum-/Session-Zuordnung). Diese liegen nur <b>flüchtig im Arbeitsspeicher</b> und werden gelöscht, sobald der Raum leer ist (spätestens nach 24&nbsp;h).</li>
<li><b>Session-Vermittlung</b>: Ein zufälliger Code + 6-stellige PIN werden temporär im Speicher gehalten (max. 24&nbsp;h).</li>
<li><b>Verbindungs-Metadaten</b>: Wie bei jedem Internetdienst sind dem Server beim Verbinden IP-Adressen technisch bekannt; sie werden nicht dauerhaft protokolliert.</li>
<li><b>TURN-Relay (nur Fallback)</b>: Falls keine direkte Verbindung möglich ist, kann verschlüsselter Audioverkehr über einen Relay laufen. Der Relay leitet nur <b>verschlüsselte Bytes</b> weiter.</li>
</ul>
<h2>Drittanbieter</h2>
<p>Installer werden über GitHub Releases bereitgestellt (beim Download gelten die Bestimmungen von GitHub). STUN/TURN kann öffentliche STUN-Server zur NAT-Erkennung nutzen.</p>
<h2>Kontakt</h2>
<p>Verantwortlich: siehe <a href="/legal?lang=de">Impressum</a>. Anfragen über <a href="https://raumdock.org">raumdock.org</a>.</p>"#;

const PRIVACY_IT: &str = r#"<h1>Privacy</h1>
<p class="muted">subraum è progettato per la minimizzazione dei dati.</p>
<h2>Cosa NON accade</h2>
<ul>
<li><b>Nessuna registrazione audio/chat.</b> Voce e testo sono peer-to-peer (DTLS-SRTP / DataChannel cifrato) e non vengono memorizzati da nessuna parte.</li>
<li><b>Nessun account</b>, nessun login, nessun tracciamento, nessuna pubblicità, nessun cookie.</li>
<li>Il server di intermediazione <b>non vede i contenuti multimediali</b>.</li>
</ul>
<h2>Cosa viene trattato</h2>
<ul>
<li><b>Signaling</b>: per connettersi, le app scambiano dati di connessione tramite il server (candidati SDP/ICE, nome visualizzato, associazione stanza/sessione). Restano solo <b>in memoria</b> e vengono eliminati quando la stanza è vuota (entro 24&nbsp;h).</li>
<li><b>Intermediazione sessione</b>: un codice casuale + PIN di 6 cifre sono tenuti temporaneamente in memoria (max 24&nbsp;h).</li>
<li><b>Metadati di connessione</b>: come ogni servizio internet, gli indirizzi IP sono tecnicamente noti alla connessione; non vengono registrati in modo persistente.</li>
<li><b>Relay TURN (solo fallback)</b>: se non è possibile una via diretta, l'audio cifrato può passare per un relay, che inoltra solo <b>byte cifrati</b>.</li>
</ul>
<h2>Terze parti</h2>
<p>Gli installer sono distribuiti via GitHub Releases. STUN/TURN può usare server STUN pubblici per il rilevamento NAT.</p>
<h2>Contatto</h2>
<p>Titolare: vedi <a href="/legal?lang=it">note legali</a>. Richieste tramite <a href="https://raumdock.org">raumdock.org</a>.</p>"#;

const PRIVACY_ES: &str = r#"<h1>Privacidad</h1>
<p class="muted">subraum está diseñado para minimizar los datos.</p>
<h2>Lo que NO ocurre</h2>
<ul>
<li><b>Sin grabación de audio/chat.</b> La voz y el texto son peer-to-peer (DTLS-SRTP / DataChannel cifrado) y no se almacenan en ningún sitio.</li>
<li><b>Sin cuentas</b>, sin inicio de sesión, sin seguimiento, sin anuncios, sin cookies.</li>
<li>El servidor de intermediación <b>nunca ve el contenido multimedia</b>.</li>
</ul>
<h2>Qué se procesa</h2>
<ul>
<li><b>Señalización</b>: para conectar, las apps intercambian datos de conexión a través del servidor (candidatos SDP/ICE, nombre mostrado, asignación de sala/sesión). Solo permanecen <b>en memoria</b> y se eliminan cuando la sala queda vacía (en 24&nbsp;h como máximo).</li>
<li><b>Intermediación de sesión</b>: un código aleatorio + PIN de 6 dígitos se guardan temporalmente en memoria (máx. 24&nbsp;h).</li>
<li><b>Metadatos de conexión</b>: como cualquier servicio de internet, las direcciones IP se conocen técnicamente al conectar; no se registran de forma persistente.</li>
<li><b>Relay TURN (solo respaldo)</b>: si no hay ruta directa, el audio cifrado puede pasar por un relay, que solo reenvía <b>bytes cifrados</b>.</li>
</ul>
<h2>Terceros</h2>
<p>Los instaladores se sirven vía GitHub Releases. STUN/TURN puede usar servidores STUN públicos para la detección de NAT.</p>
<h2>Contacto</h2>
<p>Responsable: ver <a href="/legal?lang=es">aviso legal</a>. Solicitudes vía <a href="https://raumdock.org">raumdock.org</a>.</p>"#;

const PRIVACY_FR: &str = r#"<h1>Confidentialité</h1>
<p class="muted">subraum est conçu pour la minimisation des données.</p>
<h2>Ce qui n'arrive PAS</h2>
<ul>
<li><b>Aucun enregistrement audio/chat.</b> La voix et le texte sont pair-à-pair (DTLS-SRTP / DataChannel chiffré) et ne sont stockés nulle part.</li>
<li><b>Aucun compte</b>, pas de connexion, pas de suivi, pas de publicité, pas de cookies.</li>
<li>Le serveur d'intermédiation <b>ne voit jamais le contenu multimédia</b>.</li>
</ul>
<h2>Ce qui est traité</h2>
<ul>
<li><b>Signalisation</b> : pour se connecter, les apps échangent des données de connexion via le serveur (candidats SDP/ICE, nom affiché, association salle/session). Elles restent uniquement <b>en mémoire</b> et sont supprimées dès que la salle est vide (sous 24&nbsp;h au plus).</li>
<li><b>Intermédiation de session</b> : un code aléatoire + PIN à 6 chiffres sont conservés temporairement en mémoire (max 24&nbsp;h).</li>
<li><b>Métadonnées de connexion</b> : comme tout service internet, les adresses IP sont techniquement connues à la connexion ; elles ne sont pas journalisées durablement.</li>
<li><b>Relais TURN (repli uniquement)</b> : si aucune voie directe n'est possible, l'audio chiffré peut transiter par un relais, qui ne relaie que des <b>octets chiffrés</b>.</li>
</ul>
<h2>Tiers</h2>
<p>Les installeurs sont distribués via GitHub Releases. STUN/TURN peut utiliser des serveurs STUN publics pour la découverte NAT.</p>
<h2>Contact</h2>
<p>Responsable : voir les <a href="/legal?lang=fr">mentions légales</a>. Demandes via <a href="https://raumdock.org">raumdock.org</a>.</p>"#;

const LEGAL_EN: &str = r#"<h1>Legal notice</h1>
<p>subraum is a non-commercial community project (<a href="https://raumdock.org">raumdock.org</a>).</p>
<h2>Authors</h2>
<p>head87x &amp; justcallmedeimos</p>
<h2>Provider</h2>
<p class="muted">Operator: raumdock.org<br>Contact: via <a href="https://raumdock.org">raumdock.org</a></p>
<h2>Liability</h2>
<p>The software is provided "as is", without warranty or liability (see the <a href="/license?lang=en">license</a>). The operators of linked external sites are responsible for their content.</p>"#;

const LEGAL_DE: &str = r#"<h1>Impressum / Rechtliches</h1>
<p>subraum ist ein nicht-kommerzielles Community-Projekt (<a href="https://raumdock.org">raumdock.org</a>).</p>
<h2>Autoren</h2>
<p>head87x &amp; justcallmedeimos</p>
<h2>Anbieter</h2>
<p class="muted"><!-- TODO: vollständige Anbieterkennzeichnung gemäß §5 DDG eintragen -->Verantwortlicher Betreiber: raumdock.org<br>Kontakt: über <a href="https://raumdock.org">raumdock.org</a></p>
<h2>Haftung</h2>
<p>Die Software wird „wie besehen", ohne Gewähr und ohne Haftung bereitgestellt (siehe <a href="/license?lang=de">Lizenz</a>). Für Inhalte verlinkter externer Seiten sind deren Betreiber verantwortlich.</p>"#;

const LEGAL_IT: &str = r#"<h1>Note legali</h1>
<p>subraum è un progetto di community non commerciale (<a href="https://raumdock.org">raumdock.org</a>).</p>
<h2>Autori</h2>
<p>head87x &amp; justcallmedeimos</p>
<h2>Fornitore</h2>
<p class="muted">Operatore: raumdock.org<br>Contatto: tramite <a href="https://raumdock.org">raumdock.org</a></p>
<h2>Responsabilità</h2>
<p>Il software è fornito "così com'è", senza garanzie né responsabilità (vedi <a href="/license?lang=it">licenza</a>). I gestori dei siti esterni collegati sono responsabili dei loro contenuti.</p>"#;

const LEGAL_ES: &str = r#"<h1>Aviso legal</h1>
<p>subraum es un proyecto comunitario no comercial (<a href="https://raumdock.org">raumdock.org</a>).</p>
<h2>Autores</h2>
<p>head87x &amp; justcallmedeimos</p>
<h2>Proveedor</h2>
<p class="muted">Operador: raumdock.org<br>Contacto: vía <a href="https://raumdock.org">raumdock.org</a></p>
<h2>Responsabilidad</h2>
<p>El software se ofrece "tal cual", sin garantía ni responsabilidad (ver <a href="/license?lang=es">licencia</a>). Los operadores de los sitios externos enlazados son responsables de su contenido.</p>"#;

const LEGAL_FR: &str = r#"<h1>Mentions légales</h1>
<p>subraum est un projet communautaire non commercial (<a href="https://raumdock.org">raumdock.org</a>).</p>
<h2>Auteurs</h2>
<p>head87x &amp; justcallmedeimos</p>
<h2>Fournisseur</h2>
<p class="muted">Exploitant : raumdock.org<br>Contact : via <a href="https://raumdock.org">raumdock.org</a></p>
<h2>Responsabilité</h2>
<p>Le logiciel est fourni « en l'état », sans garantie ni responsabilité (voir la <a href="/license?lang=fr">licence</a>). Les exploitants des sites externes liés sont responsables de leur contenu.</p>"#;
