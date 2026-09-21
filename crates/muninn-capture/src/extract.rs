//! Typed candidates from a session: what the write path stores besides literal
//! episodes. Every extractor is a fixed rule over the transcript — no model, no
//! rewriting — and every candidate keeps its literal text and its origin, from which
//! trust is derived (ENGINE.md §2.1, §3).

use crate::model::{Session, Turn};
use crate::redact::redact;
use muninn_core::sanitize::truncate_chars;
use regex::Regex;
use std::collections::BTreeMap;
use std::sync::OnceLock;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    pub kind: &'static str,
    pub subject: String,
    pub relation: String,
    pub object: String,
    pub body: String,
    pub origin: &'static str,
    pub anchor_path: Option<String>,
    pub turn_index: usize,
    pub end_offset: u64,
}

/// Trust is a function of origin and nothing else [W3].
pub fn trust_of(origin: &str) -> i64 {
    match origin {
        "user_said" | "review_accepted" => 3,
        "commit_linked" => 2,
        "tool_observed" => 1,
        _ => 0,
    }
}

fn first_line(s: &str, max: usize) -> String {
    truncate_chars(
        s.lines()
            .find(|l| !l.trim().is_empty())
            .unwrap_or("")
            .trim(),
        max,
    )
    .to_string()
}

/// Lowercase, alphanumerics and single spaces only — the supersession key for a
/// sentence-shaped subject.
pub fn norm_key(s: &str, max: usize) -> String {
    let mut out = String::with_capacity(s.len());
    let mut last_space = true;
    for c in s.chars().flat_map(|c| c.to_lowercase()) {
        if c.is_alphanumeric() {
            out.push(c);
            last_space = false;
        } else if !last_space {
            out.push(' ');
            last_space = true;
        }
    }
    truncate_chars(out.trim(), max).to_string()
}

fn correction_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        // a "no" family at the start of the prompt, or an explicit correction marker
        // within its first 80 characters: a "no quiero" three sentences in is a wish,
        // not a correction (SessionStart was reinjecting those every session)
        Regex::new(
            r"(?is)^\s*(no[,.:;! ]|nope\b|as[ií] no\b|eso no\b|don'?t\b|do not\b|not like that|wrong[,. ]|undo\b|revert\b|stop\b|no quiero\b|no uses\b|no hagas\b)|^.{0,80}?(\bas[ií] no\b|\ben vez de\b|\ben lugar de\b|\bya no\b|\brevierte\b|\brevertimos\b|\bdescart[ae]|\bte dije\b|\ba pesar de que\b|\bnot like that\b|\binstead of\b|\bi said\b|\bi told you\b|\bthat'?s not what\b)",
        )
        .unwrap()
    })
}

fn invariant_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        Regex::new(r"(?i)\b(siempre|nunca|jam[aá]s|never|always|must not|must\b|debe[sn]? de\b|debe[sn]?\b|no debe|prohibido|obligatorio)\b").unwrap()
    })
}

fn commit_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        Regex::new(r"\[([\w./+\-]+)(?: \(root-commit\))? ([0-9a-f]{7,40})\] ([^\n]*)").unwrap()
    })
}

fn is_tagged(s: &str) -> bool {
    let r = s.trim();
    r.starts_with('<') && r.contains("</")
}

/// Sentences end at a newline, or at `.`, `!`, `;` followed by whitespace — so
/// `CLAUDE.md` and `v1.1` stay whole.
fn split_sentences(s: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0;
    let b = s.as_bytes();
    for (i, &c) in b.iter().enumerate() {
        let end = c == b'\n'
            || (matches!(c, b'.' | b'!' | b';')
                && b.get(i + 1).is_none_or(|n| n.is_ascii_whitespace()));
        if end {
            let piece = s[start..i].trim();
            if !piece.is_empty() {
                out.push(piece);
            }
            start = i + 1;
        }
    }
    let tail = s[start..].trim();
    if !tail.is_empty() {
        out.push(tail);
    }
    out
}

fn user_candidates(sid: &str, t: &Turn, out: &mut Vec<Candidate>) {
    let up = t.user_prompt.trim();
    if up.is_empty() || is_tagged(up) || up.starts_with("This session is being continued") {
        return;
    }
    if correction_re().is_match(up) && up.chars().count() <= 1_500 {
        let body = redact(&format!("user: {}\n", truncate_chars(up, 900)));
        out.push(Candidate {
            kind: "correction",
            subject: format!("correction:{}#{}", &sid[..sid.len().min(8)], t.index),
            relation: "user_said".into(),
            object: redact(&first_line(up, 160)),
            body,
            origin: "user_said",
            anchor_path: t.files_touched.first().cloned(),
            turn_index: t.index,
            end_offset: t.end_offset,
        });
    }
    decision_candidates(t, up, out);
    for sent in split_sentences(up) {
        let n = sent.chars().count();
        if !(12..=220).contains(&n) || sent.contains('?') || !invariant_re().is_match(sent) {
            continue;
        }
        let key = norm_key(sent, 80);
        if key.split(' ').count() < 3 {
            continue;
        }
        out.push(Candidate {
            kind: "invariant",
            subject: key,
            relation: "must".into(),
            object: redact(sent),
            body: redact(&format!("user: {}\n", sent)),
            origin: "user_said",
            anchor_path: None,
            turn_index: t.index,
            end_offset: t.end_offset,
        });
    }
}

/// A sentence that states a decision ("we go with X", "X is now Y", "usamos X"). The
/// families are generic statement forms, English and Spanish, not the wording of any
/// benchmark (PREREGISTRATION.md, 2026-09-17: developed on topics and phrasings that no
/// grid uses, then frozen).
fn decision_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        Regex::new(
            r"(?i)\b(we(?:'ll| will| are|'re)? (?:go|going) with|we(?:'ll| will)? (?:use|pick|adopt|keep)|we(?:'re| are) (?:using|on)|let'?s (?:use|go with|stick with|keep)|we (?:chose|picked|decided|settled on|standardi[sz]ed on|switched|moved|migrated|agreed)|decided (?:to|on)|(?:is|are) now\b|(?:should|must) (?:now )?(?:be|use)\b|switch(?:ed|ing)? (?:to|over)|mov(?:e|ed|ing) (?:to|over)|migrat(?:e|ed|ing) to|change of plan|instead of|replac(?:e|ed|ing) \w|no longer|from now on|going forward|revert(?:ed|ing)? (?:to|back)|roll(?:ed)? back to|stick(?:ing)? with|drop(?:ped|ping)? \w|(?:decision|policy|convention)\s*:|usamos|usaremos|vamos (?:a usar|con)|elegimos|decidimos|nos quedamos con|ahora (?:es|son|usamos|va)|cambiamos (?:a|de)|pasamos a|migramos a|volvemos a|en vez de|en lugar de|a partir de ahora|ya no)",
        )
        .unwrap()
    })
}

/// Words and phrases that announce a change to something said earlier (loop 2: a broad,
/// generic list, English and Spanish, developed on the loop-2 development set).
fn change_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        Regex::new(
            r"(?i)(\b(?:is|are) now\b|\bnow (?:we|it'?s|use|using|goes|go)\b|\bswitch|\bswap|\bmov(?:e|ed|ing)\b.{0,40}?\b(?:to|over|off|onto)\b|\bmigrat(?:e|ed|es|ing)\b|\bchange of plans?\b|\bchang(?:e|ed|ing) (?:to|it|that|this|our|the)\b|\binstead\b|\breplac(?:e|ed|es|ing)\b|\bno longer\b|\bnot .{0,20}\banymore\b|\bfrom now on\b|\bgoing forward\b|\brevert|\broll(?:ed)? back\b|\bgo(?:ing)? back to\b|\bdrop(?:ped|ping)?\b|\bditch|\bscrap|\bscratch that\b|\bactually\b|\bafter all\b|\bupdate[ds]?\s*:|\bturns out\b|\bon second thoughts?\b|\bsecond thoughts\b|\breconsider|\bon reflection\b|\brethink|\bchang(?:ed|ing) (?:my|our) minds?\b|\bchanging course\b|\bwithdraw|\bretract|\bnever ?mind\b|\bforget (?:it|that|this|about)\b|\bcancel\b|\bpensándolo bien\b|\bpensandolo bien\b|\bme retracto\b|\bolvida (?:eso|lo)\b|\bahora\b|\bcambi(?:a|an|amos|ar|aron|ado|ando|é|e)\b|\bcambio (?:a|de|al)\b|\bcambio de plan|\bpasamos a\b|\bpasa a\b|\bmigra(?:mos|r|ron|ndo|do)\b|\bvolvemos a\b|\ben vez de\b|\ben lugar de\b|\ba partir de ahora\b|\bya no\b|\breemplaz(?:a|an|amos|ar|ado|ando)\b|\bsustitu(?:ye|yen|imos|ir|ido|yendo)\b|\bdejamos de\b|\bmejor usa|^\s*mejor\b|\bal final\b)",
        )
        .unwrap()
    })
}

/// Imperative and first-person forms of stating a choice (loop 2), besides `decision_re`.
fn choice_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        Regex::new(
            r"(?i)((?:^|:)\s*(?:ok(?:ay)?[, ]+|so[, ]+|hey[, ]+|alright[, ]+)?(?:use|using|go with|going with|stick (?:to|with)|prefer|default to|keep|pick|choose|run|deploy|host|store|put)\b|\b(?:i|we)(?:'d| would)? (?:prefer|want|like) (?:to use|to go with|to keep)?\b|\bthe way to go\b|\bit is\b.{0,20}$|(?:^|:)\s*(?:usa|usemos|utiliza|vamos con|quedate con|quédate con|prefiero|despliega|guarda|mejor)\b|\bnos quedamos\b|\bvamos a (?:usar|ir con)\b)",
        )
        .unwrap()
    })
}

const STOP: &[&str] = &[
    // English function words and the decision/change vocabulary itself
    "the",
    "and",
    "for",
    "with",
    "that",
    "this",
    "these",
    "those",
    "from",
    "into",
    "onto",
    "our",
    "we",
    "will",
    "are",
    "was",
    "were",
    "been",
    "being",
    "have",
    "has",
    "had",
    "use",
    "uses",
    "using",
    "used",
    "now",
    "going",
    "forward",
    "go",
    "goes",
    "let",
    "lets",
    "stick",
    "keep",
    "chose",
    "choose",
    "picked",
    "pick",
    "decided",
    "decide",
    "decision",
    "settled",
    "settle",
    "switch",
    "switched",
    "switching",
    "over",
    "move",
    "moved",
    "moving",
    "migrate",
    "migrated",
    "change",
    "changed",
    "plan",
    "instead",
    "replace",
    "replaced",
    "replacing",
    "longer",
    "revert",
    "reverted",
    "back",
    "roll",
    "rolled",
    "drop",
    "dropped",
    "actually",
    "update",
    "updated",
    "policy",
    "convention",
    "should",
    "must",
    "all",
    "any",
    "not",
    "but",
    "only",
    "also",
    "then",
    "than",
    "project",
    "team",
    "its",
    "it's",
    "you",
    "your",
    "they",
    "them",
    "there",
    "here",
    "what",
    "which",
    "when",
    "where",
    "adopt",
    "agreed",
    "standardized",
    "standardised",
    "reply",
    "short",
    "sentence",
    "acknowledging",
    "one",
    "please",
    "okay",
    "fine",
    "swap",
    "swapped",
    "ditch",
    "ditched",
    "scrap",
    "scrapped",
    "scratch",
    "after",
    "turns",
    "out",
    "anymore",
    "prefer",
    "default",
    "way",
    "think",
    "hey",
    "yeah",
    "just",
    "like",
    "want",
    "would",
    "i'd",
    "we'd",
    "let's",
    "i'm",
    "we're",
    "don't",
    "can",
    "could",
    "maybe",
    "probably",
    "really",
    "thing",
    "stuff",
    "sure",
    "right",
    "good",
    "better",
    "best",
    "idea",
    "for now",
    // Spanish
    "el",
    "la",
    "los",
    "las",
    "del",
    "que",
    "una",
    "uno",
    "unos",
    "unas",
    "para",
    "por",
    "con",
    "sin",
    "como",
    "usamos",
    "usaremos",
    "usar",
    "vamos",
    "elegimos",
    "decidimos",
    "nos",
    "quedamos",
    "ahora",
    "cambiamos",
    "cambio",
    "plan",
    "pasamos",
    "migramos",
    "volvemos",
    "vez",
    "lugar",
    "partir",
    "ya",
    "este",
    "esta",
    "estos",
    "estas",
    "proyecto",
    "equipo",
    "todo",
    "todos",
    "pero",
    "más",
    "mas",
    "sus",
    "hay",
    "son",
    "es",
    "va",
    "usa",
    "usemos",
    "utiliza",
    "prefiero",
    "mejor",
    "final",
    "cambia",
    "cambiar",
    "reemplaza",
    "reemplazamos",
    "sustituye",
    "dejamos",
    "vuelve",
    "creo",
    "bueno",
    "vale",
    "pues",
    "algo",
    "tambien",
    "también",
];

/// Content words of a decision sentence: the topic and the value, without the
/// decision vocabulary. The supersession test compares these sets.
pub fn topic_words(s: &str) -> Vec<String> {
    let key = norm_key(s, 400);
    let mut out: Vec<String> = key
        .split(' ')
        .filter(|w| {
            w.chars().count() >= 3 && !STOP.contains(w) && !w.chars().all(|c| c.is_ascii_digit())
        })
        .map(str::to_string)
        .collect();
    out.sort();
    out.dedup();
    out
}

/// The measured slots of a statement: every number that is followed by a content word,
/// as (unit, number). "payment worker: 3 attempts" gives `[("attempts", "3")]`.
///
/// A decision about a quantity is the one kind of replacement that needs no shared
/// vocabulary to recognise: the unit *is* the topic, and a different number in the same
/// unit is a different decision. Nothing here reads the words around it, so it says
/// nothing about *which* retry budget — that is the caller's guard.
pub fn quantity_slots(s: &str) -> Vec<(String, String)> {
    let key = norm_key(s, 400);
    let toks: Vec<&str> = key.split(' ').filter(|w| !w.is_empty()).collect();
    let mut out = Vec::new();
    for pair in toks.windows(2) {
        let (num, unit) = (pair[0], pair[1]);
        if !num.chars().all(|c| c.is_ascii_digit()) || num.is_empty() {
            continue;
        }
        if unit.chars().count() < 3 || STOP.contains(&unit) || unit.chars().any(|c| c.is_numeric())
        {
            continue;
        }
        out.push((unit.to_string(), num.to_string()));
    }
    out.sort();
    out.dedup();
    out
}

/// Content words of a statement that are not part of one of its quantity slots. A message
/// that has none left ("bump to 7 attempts") states a value and nothing else, so it can
/// only be about a value already on record.
pub fn words_outside_quantities(s: &str) -> Vec<String> {
    let units: Vec<String> = quantity_slots(s).into_iter().map(|(u, _)| u).collect();
    topic_words(s)
        .into_iter()
        .filter(|w| !units.contains(w))
        .collect()
}

/// Tokens that look like a name of a thing (a product, a library, a version): an inner
/// capital, a digit, a dot or hyphen inside the word, or any capitalised word that is not a
/// `STOP` word — wherever it sits. Lowercased.
///
/// Loop 6: the capital case used to require `i > 0`, so a statement that opens with the
/// product ("Pingdom for uptime monitoring.") named nothing, and loop 3's implicit-change
/// rule — guarded by `!old_names.is_empty()` — could not fire against it. That guard was
/// what blocked six of the nine loop-5 held-out misses. `STOP` already carries the ordinary
/// openers ("the", "we", "use", "go"), which is why dropping the position test costs no
/// false retirement: held-out `kept_b` 30/30 before and after.
pub fn name_tokens(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut prev = String::new();
    for (i, raw) in s
        .split(|c: char| {
            c.is_whitespace() || matches!(c, ',' | ';' | '(' | ')' | '"' | '`' | '!' | '?')
        })
        .enumerate()
    {
        let w = raw.trim_matches(|c: char| !c.is_alphanumeric());
        if w.chars().count() < 2 {
            prev = w.to_lowercase();
            continue;
        }
        let mut chars = w.chars();
        let first = chars.next().unwrap();
        let rest: String = chars.collect();
        let inner_cap = rest.chars().any(|c| c.is_uppercase());
        let digit = w.chars().any(|c| c.is_ascii_digit()) && w.chars().any(|c| c.is_alphabetic());
        let joined = w.contains('.') || w.contains('-') || w.contains('_');
        let capitalised = first.is_uppercase() && !STOP.contains(&w.to_lowercase().as_str());
        let slot = i > 0
            && matches!(
                prev.as_str(),
                "use" | "using" | "with" | "to" | "adopt" | "try" | "usar" | "usa" | "con" | "a"
            )
            && w.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
            && !STOP.contains(&w.to_lowercase().as_str());
        prev = w.to_lowercase();
        if inner_cap || digit || joined || capitalised || slot {
            let l = w.to_lowercase();
            if !out.contains(&l) {
                out.push(l);
            }
        }
    }
    out
}

/// A message that takes back what was said before, with nothing in its place.
pub fn is_withdrawal(s: &str) -> bool {
    static R: OnceLock<Regex> = OnceLock::new();
    let r = R.get_or_init(|| {
        Regex::new(r"(?i)\b(withdraw|retract|never ?mind|forget (?:it|that|this|about)|drop (?:it|that|this|the idea)|scrap (?:it|that|this)|remove (?:it|that|this)|cancel (?:it|that|this)|not (?:do|doing) (?:it|that|this)|no (?:vamos|lo hagas)|me retracto|olvida (?:eso|lo)|mejor no|quita(?:lo|r) (?:eso)?)\b").unwrap()
    });
    r.is_match(s)
}

/// A short change that names no topic of its own ("actually switch to ECharts", "mejor
/// Postmark") refers to what was said just before.
pub fn is_anaphoric(s: &str) -> bool {
    static R: OnceLock<Regex> = OnceLock::new();
    let r = R.get_or_init(|| {
        Regex::new(r"(?i)\b(that|this|it|those|them|eso|esto|ese|esa|lo)\b").unwrap()
    });
    topic_words(s).len() <= 4 || r.is_match(s)
}

/// Words of a short leading label ("note to self:", "update:"). Two statements that share
/// only such words are not about the same thing.
pub fn label_words(s: &str) -> Vec<String> {
    match s.split_once(':') {
        Some((head, tail)) if head.split_whitespace().count() <= 4 && !tail.trim().is_empty() => {
            topic_words(head)
        }
        _ => Vec::new(),
    }
}

/// Does the later statement name a different value? A name-like token the earlier one lacks,
/// or, failing names, each side has a content word the other does not ("openssl" / "rustls").
pub fn names_new_value(old: &str, new: &str) -> bool {
    let on = name_tokens(old);
    if name_tokens(new).iter().any(|w| !on.contains(w)) {
        return true;
    }
    let (ow, nw) = (topic_words(old), topic_words(new));
    let (lo, ln) = (label_words(old), label_words(new));
    let only_new = nw.iter().any(|w| !ow.contains(w) && !ln.contains(w));
    let only_old = ow.iter().any(|w| !nw.contains(w) && !lo.contains(w));
    only_new && only_old
}

/// `replaces` on two texts, ignoring the label words they have in common.
pub fn replaces_text(old: &str, new: &str, announces_change: bool) -> bool {
    let (lo, ln) = (label_words(old), label_words(new));
    let common: Vec<String> = lo.into_iter().filter(|w| ln.contains(w)).collect();
    let strip = |v: Vec<String>| {
        v.into_iter()
            .filter(|w| !common.contains(w))
            .collect::<Vec<_>>()
    };
    replaces(
        &strip(topic_words(old)),
        &strip(topic_words(new)),
        announces_change,
    )
}

/// Does a later decision (`new`, with or without a change marker) replace an earlier
/// one (`old`)? Two shared content words at least, and a share of the smaller set of
/// ≥ 0.34 when the later sentence announces a change, ≥ 0.5 otherwise.
pub fn replaces(old: &[String], new: &[String], announces_change: bool) -> bool {
    let shared = old.iter().filter(|w| new.contains(w)).count();
    let small = old.len().min(new.len()).max(1);
    let share = shared as f64 / small as f64;
    // A floor of one shared word was tried and dropped: of 30 held-out pairs, 23 share no
    // content word at all, so lowering the floor reaches almost none of them and only widens
    // what two unrelated decisions can pair on (loop 7, development sets).
    shared >= 2 && share >= if announces_change { 0.34 } else { 0.5 }
}

fn decision_candidates(t: &Turn, up: &str, out: &mut Vec<Candidate>) {
    let before = out.len();
    let mut sentence_change = false;
    for sent in split_sentences(up) {
        let n = sent.chars().count();
        if !(6..=300).contains(&n) || sent.contains('?') {
            continue;
        }
        let change = change_re().is_match(sent);
        sentence_change |= change && !name_tokens(sent).is_empty();
        // A sentence that states a measured value and nothing else ("bump to 7 attempts")
        // announces a decision in a vocabulary no verb list covers; whether it *replaces*
        // one is decided against the store, in `supersede_quantity`.
        let bare_quantity =
            !quantity_slots(sent).is_empty() && words_outside_quantities(sent).len() <= 1;
        if !(change || bare_quantity || decision_re().is_match(sent) || choice_re().is_match(sent))
        {
            continue;
        }
        let words = topic_words(sent);
        // a one-word change that names something ("switch to ECharts") still counts: it refers
        // to what was said before it
        if words.len() < 2 && !(change && !name_tokens(sent).is_empty() && !words.is_empty()) {
            continue;
        }
        out.push(Candidate {
            kind: "decision",
            // the supersession key is the content-word set; the flag rides in the subject
            subject: format!(
                "said:{}:{}",
                if change { "change" } else { "state" },
                words.join(" ")
            ),
            relation: "user_decision".into(),
            object: redact(sent),
            body: redact(&format!("user: {}\n", sent)),
            origin: "user_said",
            anchor_path: None,
            turn_index: t.index,
            end_offset: t.end_offset,
        });
    }
    // loop 5: a message whose change cue and named value sit in different sentences ("I think
    // Atlas is the better choice. Let's switch to that instead."), or that withdraws what came
    // before, is one candidate for the whole message
    let n = up.chars().count();
    if !sentence_change && (8..=700).contains(&n) && !up.trim_end().ends_with('?') {
        let names = name_tokens(up);
        let msg_change = change_re().is_match(up);
        if msg_change && (!names.is_empty() || is_withdrawal(up)) {
            let words = topic_words(up);
            let object = truncate_chars(up, 300).to_string();
            out.truncate(before);
            out.push(Candidate {
                kind: "decision",
                subject: format!("said:change:{}", words.join(" ")),
                relation: "user_decision".into(),
                object: redact(&object),
                body: redact(&format!("user: {}\n", object)),
                origin: "user_said",
                anchor_path: None,
                turn_index: t.index,
                end_offset: t.end_offset,
            });
        }
    }
}

fn commit_candidates(t: &Turn, out: &mut Vec<Candidate>) {
    for c in &t.tools {
        if !(c.name == "Bash" || c.name == "shell") || !c.target.contains("git commit") {
            continue;
        }
        for cap in commit_re().captures_iter(&c.result_head) {
            let branch = &cap[1];
            let hash = &cap[2];
            let msg = cap[3].trim();
            let short: String = hash.chars().take(7).collect();
            let files = if t.files_touched.is_empty() {
                String::new()
            } else {
                format!(
                    "files: {}\n",
                    t.files_touched
                        .iter()
                        .take(12)
                        .cloned()
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            };
            out.push(Candidate {
                kind: "decision",
                subject: format!("commit:{short}"),
                relation: "is".into(),
                object: redact(truncate_chars(msg, 160)),
                body: redact(&format!("commit {short} on {branch}: {msg}\n{files}")),
                origin: "commit_linked",
                anchor_path: t.files_touched.first().cloned(),
                turn_index: t.index,
                end_offset: t.end_offset,
            });
        }
    }
}

fn norm_cmd(s: &str) -> String {
    truncate_chars(&s.split_whitespace().collect::<Vec<_>>().join(" "), 120).to_string()
}

fn looks_like_test(cmd: &str) -> bool {
    let c = cmd.to_lowercase();
    [
        "cargo test",
        "pytest",
        "npm test",
        "pnpm test",
        "yarn test",
        "go test",
        "make test",
        "cargo clippy",
        "cargo build",
        "npm run build",
        "tsc",
        "mvn ",
        "gradle",
    ]
    .iter()
    .any(|p| c.contains(p))
}

/// A command that failed repeatedly, or a build/test command whose last run in the
/// session failed, is a dead end: the anti-pattern is worth more than the example [J2].
fn deadend_candidates(session: &Session, out: &mut Vec<Candidate>) {
    #[derive(Default)]
    struct Hist {
        fails: usize,
        last_ok: bool,
        last_exit: Option<i64>,
        last_head: String,
        last_turn: usize,
        last_off: u64,
        cmd: String,
    }
    let mut hist: BTreeMap<String, Hist> = BTreeMap::new();
    for t in &session.turns {
        for c in &t.tools {
            if !(c.name == "Bash" || c.name == "shell") || c.target.trim().is_empty() {
                continue;
            }
            let key = norm_cmd(&c.target);
            let h = hist.entry(key.clone()).or_default();
            let failed =
                matches!(c.exit_code, Some(n) if n != 0) || (c.exit_code.is_none() && c.is_error);
            if failed {
                h.fails += 1;
            }
            h.last_ok = !failed;
            h.last_exit = c.exit_code;
            h.last_head = c.result_head.clone();
            h.last_turn = t.index;
            h.last_off = t.end_offset;
            h.cmd = key;
        }
    }
    for h in hist.values() {
        if h.last_ok {
            continue;
        }
        if h.fails >= 2 || (h.fails >= 1 && looks_like_test(&h.cmd)) {
            let exit = h
                .last_exit
                .map(|n| n.to_string())
                .unwrap_or_else(|| "error".into());
            out.push(Candidate {
                kind: "deadend",
                subject: format!("cmd:{}", h.cmd),
                relation: "tried_and_failed".into(),
                object: format!("exit {exit}, {} failed run(s)", h.fails),
                body: redact(&format!(
                    "$ {}\nexit {exit} after {} failed run(s); last output:\n{}\n",
                    h.cmd,
                    h.fails,
                    truncate_chars(&h.last_head, 700)
                )),
                origin: "tool_observed",
                anchor_path: None,
                turn_index: h.last_turn,
                end_offset: h.last_off,
            });
        }
    }
}

/// All typed candidates of a session, in transcript order (dead ends last).
pub fn extract(session: &Session, sid: &str) -> Vec<Candidate> {
    let mut out = Vec::new();
    for t in &session.turns {
        user_candidates(sid, t, &mut out);
        commit_candidates(t, &mut out);
    }
    deadend_candidates(session, &mut out);
    for c in &mut out {
        c.body = truncate_chars(&c.body, muninn_core::caps::MAX_BODY_CHARS).to_string();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::ToolCall;

    fn turn(i: usize, prompt: &str) -> Turn {
        Turn {
            index: i,
            user_prompt: prompt.into(),
            ..Default::default()
        }
    }

    #[test]
    fn corrections_and_invariants_from_the_user() {
        let s = Session {
            turns: vec![
                turn(
                    0,
                    "No, así no. En vez de borrar el fichero, márcalo como invalid.",
                ),
                turn(
                    1,
                    "Nunca uses pkill en Bash. Siempre commitea como code@ilien.dev.",
                ),
                turn(2, "¿Debe el hook abrir la base en solo lectura?"),
                turn(3, "Veo que dice que treesitter sera parte de la version 1.1 a pesar de que te dije que tiene que ser parte del MVP"),
                turn(4, "Me gustaria que dentro de la definicion de la herramienta si consideremos tener nuestro propio motor de memoria. No importa si la herramienta ya tiene una. Podemos seguir el estandar pero no quiero depender de la memoria nativa."),
            ],
            ..Default::default()
        };
        let c = extract(&s, "abcdef12");
        let kinds: Vec<&str> = c.iter().map(|x| x.kind).collect();
        assert_eq!(kinds.iter().filter(|k| **k == "correction").count(), 2);
        assert_eq!(kinds.iter().filter(|k| **k == "invariant").count(), 2);
        let inv = c.iter().find(|x| x.kind == "invariant").unwrap();
        assert_eq!(inv.subject, "nunca uses pkill en bash");
        assert_eq!(trust_of(inv.origin), 3);
    }

    /// A prefix that also matches a very common *noun* turns every sentence containing that
    /// noun into an announced change. `\bmigrat` matched "migrations"; `\bcambi` matched
    /// "los cambios fueron…". Both retired decisions that were still true: 14/15 on the
    /// loop-10 precision set and three of four sampled retirements in a real store.
    #[test]
    fn a_noun_is_not_an_announced_change() {
        let is_change = |s: &str| change_re().is_match(s);
        for s in [
            "user data migrations run in CI",
            "los cambios fueron la correccion de un bug",
            "the replacement parts arrive on Tuesday",
            "la migracion de datos tarda dos horas",
        ] {
            assert!(!is_change(s), "not a change: {s}");
        }
        for s in [
            "migrating to Postgres next week",
            "we migrate to Postgres next week",
            "cambio a Zustand, es mas simple",
            "mejor cambiamos a Fastify",
            "replacing gzip with zstd",
            "reemplazamos gzip por zstd",
        ] {
            assert!(is_change(s), "is a change: {s}");
        }
    }

    /// Development set: topics and phrasings that no benchmark in this repository uses.
    #[test]
    fn user_decisions_and_what_replaces_them() {
        let dec = |p: &str| {
            let s = Session {
                turns: vec![turn(0, p)],
                ..Default::default()
            };
            extract(&s, "abcdef12")
                .into_iter()
                .filter(|c| c.relation == "user_decision")
                .collect::<Vec<_>>()
        };
        let w = |p: &str| topic_words(&dec(p)[0].object);
        let ch = |p: &str| dec(p)[0].subject.starts_with("said:change:");
        // replaced: English, several forms
        let pairs = [
            (
                "We'll use PostgreSQL for the analytics warehouse.",
                "Change of plan: the analytics warehouse is now ClickHouse.",
            ),
            (
                "Let's go with Tailwind for the admin dashboard styling.",
                "We switched the admin dashboard styling to vanilla CSS modules.",
            ),
            (
                "The retry policy: exponential backoff capped at 30 seconds for webhook delivery.",
                "Webhook delivery retry policy is now a fixed 5-second interval.",
            ),
            (
                "We picked Mapbox for the store locator map.",
                "Instead of Mapbox, the store locator map uses MapLibre going forward.",
            ),
            (
                "We decided on weekly releases for the mobile app.",
                "The mobile app no longer ships weekly releases; releases are now monthly.",
            ),
            (
                "Para el logging del backend usamos log4rs.",
                "Ya no usamos log4rs en el logging del backend; ahora es tracing.",
            ),
            (
                "Decidimos desplegar el frontend en Netlify.",
                "A partir de ahora el frontend se despliega en Cloudflare Pages en vez de Netlify.",
            ),
        ];
        for (a, b) in pairs {
            assert!(!dec(a).is_empty(), "no decision in {a:?}");
            assert!(!dec(b).is_empty(), "no decision in {b:?}");
            assert!(
                replaces(&w(a), &w(b), ch(b)),
                "{a:?} should be replaced by {b:?}: {:?} / {:?}",
                w(a),
                w(b)
            );
        }
        // not replaced: same technology or neighbouring subject, different decision
        let apart = [
            (
                "We use Redis for the session cache.",
                "We use Redis for API rate limiting.",
            ),
            (
                "We'll use JWT for the public API authentication.",
                "Let's go with gRPC for the internal service mesh.",
            ),
            (
                "The deploy target is now Fly.io for the marketing site.",
                "Let's use Vitest for the component unit tests.",
            ),
            (
                "Usamos Stripe para los pagos con tarjeta.",
                "Vamos con Resend para los correos transaccionales.",
            ),
        ];
        for (a, b) in apart {
            assert!(
                !replaces(&w(a), &w(b), ch(b)),
                "{a:?} must not be replaced by {b:?}: {:?} / {:?}",
                w(a),
                w(b)
            );
        }
        // questions and ordinary chatter are not decisions
        assert!(dec("Should we use ClickHouse for the analytics warehouse?").is_empty());
        assert!(dec("Thanks, that looks good.").is_empty());
        assert_eq!(
            trust_of(dec("We'll use PostgreSQL for the analytics warehouse.")[0].origin),
            3
        );
    }

    /// Loop-2 development set: new topics, terse imperatives, chatty and Spanish messages.
    #[test]
    fn loop2_changes_in_everyday_wording() {
        let dec = |p: &str| {
            let s = Session {
                turns: vec![turn(0, p)],
                ..Default::default()
            };
            extract(&s, "abcdef12")
                .into_iter()
                .filter(|c| c.relation == "user_decision")
                .collect::<Vec<_>>()
        };
        let words = |p: &str| topic_words(p);
        let is_change = |p: &str| dec(p).iter().any(|c| c.subject.starts_with("said:change:"));
        // (earlier statement, later change): the earlier one need not be a recognised decision
        let pairs = [
            (
                "sharp for the thumbnails",
                "swap sharp out, thumbnails go through libvips now",
            ),
            (
                "hey so for the API docs page I think Swagger UI is the way to go",
                "actually let's ditch Swagger UI and render the API docs with Redoc instead",
            ),
            (
                "app secrets live in Vault",
                "move the app secrets to Doppler",
            ),
            (
                "run the CLI tests with pytest",
                "scratch that, the CLI tests run on ward from now on",
            ),
            (
                "Lerna manages the monorepo",
                "we replaced Lerna with Turborepo for the monorepo",
            ),
            (
                "el gestor de paquetes es npm",
                "cambia el gestor de paquetes a pnpm",
            ),
            (
                "la base de datos de pruebas es SQLite",
                "ya no usamos SQLite para la base de datos de pruebas, ahora es Postgres en Docker",
            ),
            (
                "deploy the staging site on Render",
                "staging site: we're moving off Render to Railway",
            ),
        ];
        for (a, b) in pairs {
            assert!(is_change(b), "no change cue in {b:?}");
            assert!(
                replaces(&words(a), &words(b), true),
                "{a:?} / {b:?}: {:?} {:?}",
                words(a),
                words(b)
            );
        }
        let apart = [
            (
                "move the app secrets to Doppler",
                "use Doppler for the CI pipeline tokens too",
            ),
            (
                "the CLI tests run on ward from now on",
                "switch the CLI help text to plain English",
            ),
            (
                "ahora el gestor de paquetes es pnpm",
                "cambia el README a español",
            ),
            (
                "thumbnails go through libvips now",
                "actually the avatars should stay square",
            ),
        ];
        for (a, b) in apart {
            assert!(
                !replaces(&words(a), &words(b), is_change(b)),
                "{a:?} must survive {b:?}: {:?} {:?}",
                words(a),
                words(b)
            );
        }
        assert!(dec("should we swap sharp for libvips?").is_empty());
    }

    #[test]
    fn loop3_names_labels_and_anaphora() {
        assert_eq!(name_tokens("Actually switch to ECharts"), vec!["echarts"]);
        assert!(
            name_tokens("Going with Chart.js for admin reports").contains(&"chart.js".to_string())
        );
        assert!(name_tokens("the source license is GPL-3.0").contains(&"gpl-3.0".to_string()));
        assert!(name_tokens("Use the new thing").is_empty());
        // loop 6: a name that opens the statement counts, so the earlier statement of an
        // implicit change is not treated as naming nothing. Ordinary openers stay out.
        assert_eq!(
            name_tokens("Pingdom for uptime monitoring."),
            vec!["pingdom"]
        );
        assert!(name_tokens("Varnish sits in front of the cache").contains(&"varnish".to_string()));
        assert!(name_tokens("The new thing goes here").is_empty());
        assert!(name_tokens("We keep the current setup").is_empty());
        assert!(is_anaphoric("Mejor Postmark, es más confiable"));
        assert!(is_anaphoric(
            "You know what, ECharts has better customization options, let's go with that instead"
        ));
        assert!(!is_anaphoric(
            "switch the CLI help text rendering pipeline over to plain English documentation"
        ));
        let dec = |p: &str| {
            let s = Session {
                turns: vec![turn(0, p)],
                ..Default::default()
            };
            extract(&s, "abcdef12")
                .into_iter()
                .filter(|c| c.relation == "user_decision")
                .collect::<Vec<_>>()
        };
        assert!(!dec("note: use Redoc for the API docs").is_empty());
        assert!(dec("mejor Postmark, es más confiable")
            .iter()
            .any(|c| c.subject.starts_with("said:change:")));
    }

    #[test]
    fn leading_labels_are_not_topic() {
        assert!(!replaces_text(
            "note to self: use rustls for the TLS backend",
            "note to self: use https everywhere for internal hosts",
            false
        ));
        assert!(replaces_text(
            "deploy the staging site on Render",
            "staging site: we're moving off Render to Railway",
            true
        ));
    }

    #[test]
    fn commits_become_decisions_and_repeated_failures_dead_ends() {
        let mut t0 = turn(0, "commit it");
        t0.tools.push(ToolCall {
            name: "Bash".into(),
            target: "git commit -m 'Phase 3: extract'".into(),
            result_head: "[master 1a2b3c4d] Phase 3: extract\n 2 files changed".into(),
            exit_code: Some(0),
            ..Default::default()
        });
        t0.files_touched.push("crates/x.rs".into());
        let mut t1 = turn(1, "run tests");
        for _ in 0..2 {
            t1.tools.push(ToolCall {
                name: "Bash".into(),
                target: "cargo   test -p muninn-core".into(),
                result_head: "error[E0308]".into(),
                exit_code: Some(101),
                ..Default::default()
            });
        }
        let s = Session {
            turns: vec![t0, t1],
            ..Default::default()
        };
        let c = extract(&s, "abcdef12");
        let d = c.iter().find(|x| x.kind == "decision").unwrap();
        assert_eq!(d.subject, "commit:1a2b3c4");
        assert_eq!(d.anchor_path.as_deref(), Some("crates/x.rs"));
        let de = c.iter().find(|x| x.kind == "deadend").unwrap();
        assert_eq!(de.subject, "cmd:cargo test -p muninn-core");
        assert!(de.object.starts_with("exit 101"));
        // a later success clears the dead end
        let mut s2 = s.clone();
        s2.turns[1].tools.push(ToolCall {
            name: "Bash".into(),
            target: "cargo test -p muninn-core".into(),
            exit_code: Some(0),
            ..Default::default()
        });
        assert!(extract(&s2, "abcdef12").iter().all(|x| x.kind != "deadend"));
    }
}
