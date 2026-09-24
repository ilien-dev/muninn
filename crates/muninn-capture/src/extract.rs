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
    /// What the assistant said back in the same turn. Empty unless this is a change.
    pub ack: String,
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

/// "I don't understand" opens with the same `no` the correction markers look for, and it is
/// a request to explain, not a correction of anything. Measured on this project's 240 real
/// user messages: three match the correction opener and **two of them are this**.
fn asks_for_explanation(s: &str) -> bool {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        Regex::new(
            r"(?i)^\s*(?:no\s+(?:entiend|entend|comprend|me\s+queda\s+claro|s[eé]\b|tengo\s+claro)|i\s+(?:don'?t|do\s+not)\s+(?:understand|get|follow)\b|not\s+sure\s+i\s+(?:understand|follow))",
        )
        .unwrap()
    })
    .is_match(s)
}

/// The user correcting something, which is not the user asking what you meant.
fn is_correction(s: &str) -> bool {
    correction_re().is_match(s) && !asks_for_explanation(s)
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
/// The word before this byte, letters only, or empty when there is none.
fn word_before(b: &[u8], i: usize) -> &[u8] {
    let mut j = i;
    while j > 0 && b[j - 1].is_ascii_alphabetic() {
        j -= 1;
    }
    &b[j..i]
}

/// The first non-space byte after this one.
fn next_visible(b: &[u8], i: usize) -> Option<u8> {
    b[i + 1..]
        .iter()
        .find(|c| !c.is_ascii_whitespace())
        .copied()
}

fn split_sentences(s: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0;
    let b = s.as_bytes();
    let mut depth = 0i32;
    for (i, &c) in b.iter().enumerate() {
        match c {
            b'(' => depth += 1,
            b')' => depth = (depth - 1).max(0),
            _ => {}
        }
        // An abbreviation's full stop is not the end of a sentence. This project's own store
        // holds `CHECKOUT debe llamarse igual (p` — the whole of a record, cut at the `p.` of
        // `p. ej.` — and the same break splits `e.g.`, `i.e.`, `vs.` and `cf.`. A stop that
        // closes a word of three letters or fewer *and* is not followed by a capital is an
        // abbreviation: what comes after `ej.` is a backtick, not a lowercase letter, and what
        // comes after `así no.` is `Nunca`, which is where a sentence really does begin.
        let abbrev = c == b'.'
            && (1..=3).contains(&word_before(b, i).len())
            && next_visible(b, i).is_some_and(|n| !n.is_ascii_uppercase());
        // Nor is a stop inside an unclosed parenthesis: this store holds an invariant that
        // begins `…'`) inyectados en evento`, the tail of a sentence whose opening bracket was
        // split away from it. A newline still ends a line whatever is open.
        let end = c == b'\n'
            || (matches!(c, b'.' | b'!' | b';')
                && !abbrev
                && depth == 0
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

/// The user's own words, with the text they quoted taken out.
///
/// A coding session is full of pasted material: logs, documents, another tool's output, a
/// snippet under discussion. Its sentences are not things the user decided, and capturing them
/// as such puts them on record at trust 3. Measured on five of this project's own transcripts,
/// 14 decisions were captured and about ten were pasted text — one of them a line of
/// **claude-mem's own output** that had been pasted into the chat, stored as a decision of this
/// project.
///
/// Two shapes carry quotation and nothing else, so both come out before extraction: a fenced
/// block and a `>` blockquote. The episode keeps the message whole — a literal excerpt is
/// supposed to be literal; this is only about what may become a typed record.
fn unquoted(s: &str) -> String {
    let mut out = String::new();
    let mut fenced = false;
    for line in s.lines() {
        let t = line.trim_start();
        if t.starts_with("```") || t.starts_with("~~~") {
            fenced = !fenced;
            continue;
        }
        if fenced || t.starts_with('>') {
            continue;
        }
        out.push_str(line);
        out.push('\n');
    }
    out
}

/// Is this message a conversation pasted into it?
///
/// A person does not label their own lines `USER:`. Two or more such labels mean the
/// message carries a transcript — a benchmark payload replayed inside this repository, a
/// log pasted for inspection — and every rule inside belongs to that conversation, not to
/// this project. This store held "When action=choose, channel must be NONE" and three more
/// like it as project invariants; they are lines of a scaffold's JSON contract, inside a
/// PM-Bench payload whose every block starts with `USER:`.
///
/// `unquoted` already drops fenced blocks and `>` quotes for the same reason. This is the
/// third form the same thing takes, and the one a paste from a chat window arrives in.
fn is_pasted_conversation(s: &str) -> bool {
    static R: OnceLock<Regex> = OnceLock::new();
    let r = R.get_or_init(|| {
        Regex::new(r"(?im)^[ \t]*(?:user|assistant|system|human|usuario|asistente)[ \t]*:").unwrap()
    });
    r.find_iter(s).count() >= 2
}

/// Is this message a specification for text it is asking to be produced?
///
/// The sentences inside such a message constrain what is being written, not the project.
/// This store held "The replacement must not contain the original as a substring" and "In
/// each pair the two messages are about DIFFERENT things that share TWO OR MORE ordinary
/// content words" as project invariants — true of a fixture, false of the repository, and
/// delivered at every session start. They came from the prompts that generated loops 8, 9
/// and 10, which opened "Invent 10 technical decisions…" and "Write pairs of short
/// messages… Make 15 pairs".
///
/// Three conditions, all structural rather than topical, because any one of them alone is
/// ordinary: the message opens with an imperative to produce; it asks for a count of the
/// things produced; and it is long enough to be a specification. "Write 3 tests for the
/// parser" is none of those together, and keeps whatever rule it states.
fn asks_for_generated_text(s: &str) -> bool {
    static OPEN: OnceLock<Regex> = OnceLock::new();
    static COUNT: OnceLock<Regex> = OnceLock::new();
    if s.chars().count() < 400 {
        return false;
    }
    let open = OPEN.get_or_init(|| {
        Regex::new(
            r"(?i)^\s*(?:please\s+)?(?:write|make|generate|invent|produce|compose|draft|list|escribe|escribí|genera|inventa|redacta|crea|haz)\b",
        )
        .unwrap()
    });
    // a number, then a plural noun within two words of it: "15 pairs", "10 technical
    // decisions", "30 escenarios distintos"
    let count = COUNT
        .get_or_init(|| Regex::new(r"(?i)\b\d{1,4}\s+(?:\p{L}+\s+){0,2}\p{L}{3,}s\b").unwrap());
    open.is_match(s) && count.is_match(s)
}

fn user_candidates(sid: &str, t: &Turn, out: &mut Vec<Candidate>) {
    let up = t.user_prompt.trim();
    if up.is_empty() || is_tagged(up) || up.starts_with("This session is being continued") {
        return;
    }
    if is_correction(up) && up.chars().count() <= 1_500 {
        let body = redact(&format!("user: {}\n", truncate_chars(up, 900)));
        out.push(Candidate {
            kind: "correction",
            ack: String::new(),
            subject: format!("correction:{}#{}", &sid[..sid.len().min(8)], t.index),
            relation: "user_said".into(),
            object: redact(&first_line(up, 160)),
            body,
            origin: "user_said",
            anchor_path: crate::sole_anchor(&t.files_touched),
            turn_index: t.index,
            end_offset: t.end_offset,
        });
    }
    // typed records come from what the user wrote, not from what they pasted
    let own = unquoted(up);
    let own = own.trim();
    decision_candidates(t, own, out);
    if asks_for_generated_text(own) || is_pasted_conversation(own) {
        return;
    }
    for sent in split_sentences(own) {
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
            ack: String::new(),
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
            r"(?i)(\b(?:is|are) now\b|\bnow (?:we|it'?s|use|using|goes|go)\b|\bswitch(?:ed|ing)\b|\bswitch\b.{0,20}?\b(?:to|over|back|out)\b|\b(?:let'?s|lets|we'?ll|i'?ll|time to) switch\b|\bswap(?:ped|ping)\b|\bswap\b.{0,30}?\b(?:to|for|with|out|in)\b|\bmov(?:e|ed|ing)\b.{0,40}?\b(?:to|over|off|onto)\b|\bmigrat(?:e|ed|es|ing)\b|\bchange of plans?\b|\bchang(?:e|ed|ing) (?:to|it|that|this|our|the)\b|\binstead\b|\breplac(?:e|ed|es|ing)\b|\bno longer\b|\bnot .{0,20}\banymore\b|\bfrom now on\b|\bgoing forward\b|\brevert|\broll(?:ed)? back\b|\bgo(?:ing)? back to\b|\bdrop(?:ped|ping)?\b|\bditch|\bscrap|\bscratch that\b|\bactually\b|\bafter all\b|\bupdate[ds]?\s*:|\bturns out\b|\bon second thoughts?\b|\bsecond thoughts\b|\breconsider|\bon reflection\b|\brethink|\bchang(?:ed|ing) (?:my|our) minds?\b|\bchanging course\b|\bwithdraw|\bretract|\bnever ?mind\b|\bforget (?:it|that|this|about)\b|\bcancel\b|\bpensándolo bien\b|\bpensandolo bien\b|\bme retracto\b|\bolvida (?:eso|lo)\b|\bahora\b(?:\s+\w+){0,2}?\s+(?:es|son|usamos|usaremos|usa|usan|va|vamos|toca|queda|quedan|se usa|se usan|sera|será)\b|\bcambi(?:a|an|amos|ar|aron|ado|ando|é|e)\b|\bcambio (?:a|de|al)\b|\bcambio de plan|\bpasamos a\b|\bpasa a\b|\bmigra(?:mos|r|ron|ndo|do)\b|\bvolvemos a\b|\ben vez de\b|\ben lugar de\b|\ba partir de ahora\b|\bya no\b|\breemplaz(?:a|an|amos|ar|ado|ando)\b|\bsustitu(?:ye|yen|imos|ir|ido|yendo)\b|\bdejamos de\b|\bmejor usa|^\s*mejor\b|\bal final\b)",
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
    // The list held `the`, `and`, `for`, `with`, `that` and `this` but not `to`, `of`, `in`,
    // `on`, `at`, `by`, `is`, `as`, `it`, `a`, `an`, `or`, `no`, `be`, `so`, `if`, `do` or
    // `up`. Every consumer wants them gone, and one of them retires records: `ack_replacement`
    // hands `supersede_via_ack` a word and it invalidates the shortest active decision that
    // contains it, so a preposition coming back from a pattern retires whatever record is
    // shortest. `it` did exactly that to the current serialization decision. The rest were
    // latent and are closed here rather than one at a time as each is found.
    //
    // `off` is deliberately not on this list: "verification off in dev builds" is a sentence
    // where `off` is the value, and adding it took ten retirements off one fixture's stores.
    "to",
    "of",
    "in",
    "on",
    "at",
    "by",
    "is",
    "as",
    "it",
    "a",
    "an",
    "or",
    "no",
    "be",
    "so",
    "if",
    "do",
    "up",
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
    // The Spanish half was missing the counterparts of function words the English half
    // already drops. `tiene` is the one that showed: "mejor usamos Supavisor, tiene mejor
    // rendimiento" and "mejor Redpanda, tiene mejor rendimiento" are decisions about a
    // connection pooler and a message broker, and they shared exactly the two content words
    // `replaces` asks for — both of them filler. The pooler decision was retired by the
    // broker one, and with it the only record that held its value.
    // have / has / had
    "tiene",
    "tienen",
    "tener",
    "tenemos",
    "tengo",
    "tenía",
    "tenia",
    // be, in the forms a decision sentence uses
    "está",
    "esta",
    "están",
    "estan",
    "estar",
    "estamos",
    "estoy",
    "ser",
    "sea",
    "sean",
    "sería",
    "seria",
    "será",
    "sera",
    "serán",
    "seran",
    "era",
    "eran",
    "fue",
    "fueron",
    "siendo",
    // can / could / must / should / need
    "puede",
    "pueden",
    "podemos",
    "puedo",
    "poder",
    "podría",
    "podria",
    "podríamos",
    "podriamos",
    "debe",
    "deben",
    "debemos",
    "debería",
    "deberia",
    "necesita",
    "necesitan",
    "necesitamos",
    "necesito",
    // do / make
    "hacer",
    "hace",
    "hacen",
    "hacemos",
    "haremos",
    // what / which / when / where / why, and the adverbs the English half drops as
    // "really", "probably", "maybe", "just"
    "qué",
    "cual",
    "cuál",
    "cuales",
    "cuáles",
    "cuando",
    "cuándo",
    "donde",
    "dónde",
    "porque",
    "porqué",
    "muy",
    "solo",
    "sólo",
    "solamente",
    "quizá",
    "quizás",
    "tal",
    "bien",
    "entonces",
    "luego",
    // over / after / out / between, and the determiners left out of the first pass
    "sobre",
    "después",
    "despues",
    "antes",
    "entre",
    "desde",
    "hasta",
    "cada",
    "otro",
    "otra",
    "otros",
    "otras",
    "mismo",
    "misma",
    "mismos",
    "mismas",
    "cosa",
    "cosas",
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
/// The value an acknowledgement says was replaced, when it says so in so many words.
///
/// `[Z5]`: 23 of 30 held-out replacements share no content word with what they replace, so no
/// lexical test can pair them, and on the plain head-to-head 14 of Muninn's 15 failing cells
/// were a stale value served as current. But the assistant's reply in that same turn routinely
/// names both — "Got it — switching the TLS backend from openssl to rustls" — and that reply
/// is already captured. This reads only the shape that states a pair: `from X to Y`,
/// `X instead of Y`, `de X a Y`, `en vez de X`. Nothing is inferred from words merely sharing a
/// sentence; if the ack does not say it, this returns nothing.
///
/// `new_object` is what the user's own sentence decided. The pair is only used when the ack's
/// *to* side is part of it, which is what ties the sentence to this change rather than to some
/// other one the assistant mentioned.
pub fn ack_replacement(ack: &str, new_object: &str) -> Option<String> {
    static TWO: OnceLock<Vec<Regex>> = OnceLock::new();
    static ONE: OnceLock<Vec<Regex>> = OnceLock::new();
    // Every pattern below needs one of these words, and most replies have none of them. The
    // regex sets are the expensive part of the write path now that `decision_candidates`
    // asks this of a message that matched nothing else — which is most messages. Without
    // this, `s14_maintain_and_stop_concurrent` went from well under its two seconds to 2.51.
    // One entry per pattern below, and ` de ` with both spaces because the Spanish two-sided
    // shape is `de X a Y` and a bare `de` sits inside `code`, `made` and `decide`.
    const TRIGGERS: [&str; 13] = [
        "replac", "instead", "supersed", "revers", "overrid", "from ", " de ", "en vez",
        "en lugar", "sustitu", "reemplaz", "revierte", "anula",
    ];
    let low = ack.to_ascii_lowercase();
    if !TRIGGERS.iter().any(|t| low.contains(t)) {
        return None;
    }
    // two-sided: the replaced value first, the replacement second
    // `(?:\([^)]{0,60}\)\s*)?` — one short parenthetical between the value and the preposition.
    // The assistant writes "switching from gzip (the earlier decision, #3) to zstd" and
    // "switching versioning from calver (decision #23) to semver"; without this the pattern
    // needs whitespace there and sees neither. Those two scenarios are 12 of the 12 cells the
    // plain head-to-head fails, the same two in all six runs, and their replacement messages
    // — "Benchmarks show zstd is faster - let's switch", "semver is cleaner" — share no
    // content word with what they replace, so the ack is the only thing that names the pair.
    let two = TWO.get_or_init(|| {
        [
            r"(?i)\bfrom\s+([\w./@+-]{2,40})\s*(?:\([^)]{0,60}\)\s*)?\s*(?:to|over to|across to)\s+([\w./@+-]{2,40})",
            r"(?i)\breplac(?:e|ed|ing)\s+([\w./@+-]{2,40})\s*(?:\([^)]{0,60}\)\s*)?\s*with\s+([\w./@+-]{2,40})",
            r"(?i)\bde\s+([\w./@+-]{2,40})\s*(?:\([^)]{0,60}\)\s*)?\s*a\s+([\w./@+-]{2,40})",
        ]
        .iter()
        .map(|p| Regex::new(p).unwrap())
        .collect()
    });
    // The same shape with the earlier value in quotes, which is what an assistant writes when
    // that value is a phrase rather than a token: `supersedes the earlier "We're using bcrypt"
    // note on record`. The patterns above capture `[\w./@+-]`, so an opening quote stops them
    // dead and the whole reply reads as saying nothing. The quotes are also what makes this
    // safe to read as a phrase: the assistant marked where the value starts and ends, so the
    // name inside it is taken rather than guessed.
    static QUOTED: OnceLock<Vec<Regex>> = OnceLock::new();
    let quoted = QUOTED.get_or_init(|| {
        [
            "(?i)\\b(?:replaces?|replacing|supersedes?|superseding|reverses?|reversing|overrides?)\
             \\s+(?:the\\s+)?(?:earlier|previous|prior|old|former)\\s+[\"\u{201c}\u{ab}`]([^\"\u{201d}\u{bb}`]{2,60})[\"\u{201d}\u{bb}`]",
            "(?i)\\b(?:reemplaza|sustituye|revierte|anula)\\s+(?:la|el)?\\s*(?:decisi[o\u{f3}]n\\s+)?\
             (?:anterior|previa|previo)\\s+(?:de\\s+)?[\"\u{201c}\u{ab}`]([^\"\u{201d}\u{bb}`]{2,60})[\"\u{201d}\u{bb}`]",
        ]
        .iter()
        .map(|p| Regex::new(p).unwrap())
        .collect()
    });

    // `<arrived> replaces <gone>`, with no `the earlier` in it: "tokio replaces async-std as
    // the async runtime", "Apache-2.0 supersedes GPL-3.0". The one-sided set needs
    // `earlier|previous|…` because there only one value is named and that word is what makes
    // the phrase point backwards. Here both are named and the verb itself carries the
    // direction, so the word is not needed and requiring it lost the shape entirely — three
    // cells of one fixture and six of another, every one of them the assistant stating the
    // pair outright. The groups are the reverse of the two-sided set above, which is why this
    // is its own set rather than another pattern in it.
    static REVERSED: OnceLock<Vec<Regex>> = OnceLock::new();
    let reversed = REVERSED.get_or_init(|| {
        [
            r"(?i)\b([\w./@+-]{2,40})\s+(?:replaces|supersedes|overrides|reverses)\s+(?:the\s+)?([\w./@+-]{2,40})",
            r"(?i)\b([\w./@+-]{2,40})\s+(?:reemplaza|sustituye|anula)\s+(?:a\s+)?(?:la|el)?\s*([\w./@+-]{2,40})",
        ]
        .iter()
        .map(|p| Regex::new(p).unwrap())
        .collect()
    });
    // one-sided: only the replaced value is named, so what ties the sentence to this decision
    // is the replacement appearing *before* the phrase
    let one = ONE.get_or_init(|| {
        [
            // `instead of the bcrypt decision that's on record (#10)` — the article is what
            // the capture hit, and `the` is a stop word, so the whole reply read as saying
            // nothing and the earlier decision stayed active beside its replacement.
            r"(?i)\binstead of\s+(?:the\s+|our\s+|that\s+)?([\w./@+-]{2,40})",
            r"(?i)\b(?:en vez de|en lugar de)\s+(?:la\s+|el\s+|nuestro\s+|nuestra\s+)?([\w./@+-]{2,40})",
            // `<arrived> replaces the earlier <gone>` — the reverse of `replace X with Y`,
            // and the form an assistant actually writes when it is acknowledging a change
            // it has just been told about. Every failing scenario of v22 has it in every
            // run. The `earlier|previous|…` is not decoration: it is what makes the phrase
            // refer back to something on record rather than forward to anything.
            // The value can sit behind a filler noun and whatever verb the assistant chose —
            // "replaces the earlier decision to stick with openssl" — and this goes first
            // because the general shape below would capture `decision` or `choice` instead.
            // A bare `to` straight after the noun is not one of the ways in: `choice of X` and
            // `decision to <verb> with X` are, and `plan to turn it off in dev builds` is the
            // same shape with a verb where the value would be. Allowing it returned `turn`,
            // and before `usable` had a floor it returned `it`, which retired the current
            // serialization decision because that word is in its sentence.
            r"(?i)\b(?:replaces?|replacing|supersedes?|superseding|reverses?|reversing|overrides?)\s+(?:the\s+)?(?:earlier|previous|prior|old|former)\s+(?:recorded\s+|existing\s+|standing\s+|current\s+|original\s+|stated\s+)?(?:decision|choice|plan|policy|call|one|note|record|entry|setting|value)\b(?:\s+to\s+[\w-]+\s+(?:with|to|on|of|for)|\s+(?:with|on|of|for))?\s+([\w./@+-]{2,40})",
            r"(?i)\b(?:replaces?|replacing|supersedes?|superseding|reverses?|reversing|overrides?)\s+(?:the\s+)?(?:earlier|previous|prior|old|former)\s+([\w./@+-]{2,40})",
            r"(?i)\b(?:reemplaza|sustituye|revierte|anula)\s+(?:la|el)?\s*(?:decisi[oó]n\s+)?(?:anterior|previa|previo)\s+(?:de\s+)?([\w./@+-]{2,40})",
        ]
        .iter()
        .map(|p| Regex::new(p).unwrap())
        .collect()
    });
    let newn: Vec<String> = topic_words(new_object);
    let norm = |w: &str| {
        w.trim_matches(|c: char| !c.is_alphanumeric())
            .to_lowercase()
    };
    let names_new = |t: &str| {
        let low = t.to_lowercase();
        newn.iter().any(|w| low.contains(w.as_str()))
    };
    // What comes back retires a record: `supersede_via_ack` looks for an active decision with
    // this word in it and invalidates the shortest match. So a word that is in half the
    // sentences in the store is not a value, it is a way to retire the wrong record.
    //
    // It happened, and on the current decision. "…which replaces the earlier plan to turn it
    // off in dev builds" gave `it`, which is a whole word of "Switching over to cbor, better
    // type safety and **it** handles our schema better", and the serialization decision was
    // retired by the certificate one — five of five cells of that fixture's wire-format task.
    // `it` was simply missing from `STOP`. It also earns its place twice over: an assistant
    // that writes "tell me if argon2id replaces it" is asking, not saying, and that sentence
    // was being read as a retirement.
    //
    // The nouns the patterns above step over are excluded here for the same reason. A floor of
    // three characters was tried instead and measured on the thirty seeded stores: it takes
    // ten retirements off one fixture and six typed records off another, because values that
    // short are real — `off` in "verification off in dev builds" is the value. The floor stays
    // at two and the list does the work.
    // The nouns the patterns step over on the way to the value, and the words that mark the
    // phrase as pointing backwards. Neither is ever the value, and both sit exactly where one
    // would if a pattern reached one token too far.
    const FILLER: [&str; 16] = [
        "decision", "choice", "plan", "policy", "call", "one", "note", "record", "entry",
        "setting", "value", "earlier", "previous", "prior", "old", "former",
    ];
    let usable = |gone: String| {
        (gone.chars().count() >= 2
            && !STOP.contains(&gone.as_str())
            && !FILLER.contains(&gone.as_str())
            && !newn.contains(&gone))
        .then_some(gone)
    };
    for re in reversed {
        for cap in re.captures_iter(ack) {
            let arrived = norm(cap.get(1)?.as_str());
            let gone = norm(cap.get(2)?.as_str());
            // "argon2id replaces the earlier bcrypt decision" matches this shape too, and what
            // sits where the value should be is the back-reference. That sentence is the
            // one-sided set's, and it reads it correctly, so this one steps aside rather than
            // returning `earlier` as the value that went.
            const BACKREF: [&str; 6] = ["the", "earlier", "previous", "prior", "old", "former"];
            if BACKREF.contains(&gone.as_str()) {
                continue;
            }
            if !newn
                .iter()
                .any(|w| *w == arrived || arrived.contains(w.as_str()))
            {
                continue;
            }
            if let Some(g) = usable(gone) {
                return Some(g);
            }
        }
    }

    for re in two {
        for cap in re.captures_iter(ack) {
            let gone = norm(cap.get(1)?.as_str());
            let arrived = norm(cap.get(2)?.as_str());
            // the pair has to be about *this* decision, or an assistant mentioning some other
            // migration in passing would retire a record nobody touched
            if !newn
                .iter()
                .any(|w| *w == arrived || arrived.contains(w.as_str()))
            {
                continue;
            }
            if let Some(g) = usable(gone) {
                return Some(g);
            }
        }
    }
    for re in quoted {
        for cap in re.captures_iter(ack) {
            let m = cap.get(0)?;
            if !names_new(&ack[..m.start()]) {
                continue;
            }
            // Inside the quotes is a phrase, and what retires a record is one word of it. The
            // name is the one to take — `bcrypt` out of "We're using bcrypt", `LFU` out of
            // "LFU eviction" — and a phrase with no name in it names no value, so it is left.
            let phrase = cap.get(1)?.as_str();
            if let Some(g) = name_tokens(phrase).into_iter().next().and_then(usable) {
                return Some(g);
            }
        }
    }
    for re in one {
        for cap in re.captures_iter(ack) {
            let m = cap.get(0)?;
            if !names_new(&ack[..m.start()]) {
                continue;
            }
            if let Some(g) = usable(norm(cap.get(1)?.as_str())) {
                return Some(g);
            }
        }
    }
    None
}

/// Does the acknowledgement say this message replaced something, whether or not it names what?
///
/// `ack_replacement` answers a narrower question — *which* value went — because that is what
/// retires a record, and it returns nothing when the reply names the thing it replaced in a
/// clause rather than a token: "which replaces the earlier note that plain http was fine for
/// internal hosts". There is no value in that sentence to hand a retirement, and there is no
/// doubt at all that the assistant said a replacement happened.
///
/// Those are two questions and they were being answered by one function, which is how the
/// second one came to be answered by accident: the general pattern captured the filler noun
/// itself, `note`, no record contained that word so nothing was retired, and the junk value's
/// only effect was to let this message be typed. Naming the noun as a noun took the typing
/// with it. This is the question the typing rule actually asks.
///
/// Tied to this decision the same way: the value the user decided appears before the phrase.
pub fn ack_states_replacement(ack: &str, new_object: &str) -> bool {
    if ack_replacement(ack, new_object).is_some() {
        return true;
    }
    static PHRASE: OnceLock<Regex> = OnceLock::new();
    let phrase = PHRASE.get_or_init(|| {
        Regex::new(
            r#"(?i)\b(?:replaces?|replacing|supersedes?|superseding|reverses?|reversing|overrides?)\s+(?:the\s+)?(?:earlier|previous|prior|old|former)\s+(?:(?:recorded\s+|existing\s+|standing\s+|current\s+|original\s+|stated\s+)?(?:decision|choice|plan|policy|call|one|note|record|entry|setting|value)\b|["“«`][^"”»`]{2,60}["”»`])"#,
        )
        .unwrap()
    });
    let newn = topic_words(new_object);
    if newn.is_empty() {
        return false;
    }
    phrase.find_iter(ack).any(|m| {
        let before = ack[..m.start()].to_lowercase();
        newn.iter().any(|w| before.contains(w.as_str()))
    })
}

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

/// Does this text announce a change — and is the announcement not a denial of one?
///
/// `No cambié nada` ("I changed nothing") matched `\bcambi…\b` and became a `said:change:`
/// record, which is the kind that supersedes. It was found in this project's own store, along
/// with `verificado que ya no aparece`. A marker with a negation immediately in front of it
/// says the opposite of what the marker means.
fn denied(s: &str, at: usize) -> bool {
    static NEG: OnceLock<Regex> = OnceLock::new();
    let neg = NEG.get_or_init(|| {
        Regex::new(r"(?i)(?:\bno\b|\bnot\b|n't|\bnunca\b|\btampoco\b|\bsin\b)\s*$").unwrap()
    });
    // the twelve characters in front of the marker are enough for `no `, `not `, `didn't `
    let mut lo = at.saturating_sub(12);
    while lo < at && !s.is_char_boundary(lo) {
        lo += 1;
    }
    neg.is_match(&s[lo..at])
}

fn announces_change(s: &str) -> bool {
    change_re().find(s).is_some_and(|m| !denied(s, m.start()))
}

/// A decision stated outright, and not denied: `switch to X` counts, `I didn't switch to X`
/// does not, and both reach here through `decision_re` rather than the change markers.
///
/// `switch` on its own needed something after it — `to`, `over`, `back`, `out` — because a
/// bare one is as often a noun. "Benchmarks show zstd is faster - let's switch" ends on it
/// and announced nothing, so no decision was captured, so the reply that named the pair
/// outright was never consulted: six of the twelve cells the plain head-to-head fails. A
/// first person proposing to switch is not a noun in any of these forms.
fn states_decision(s: &str) -> bool {
    decision_re()
        .find(s)
        .or_else(|| choice_re().find(s))
        .is_some_and(|m| !denied(s, m.start()))
}

fn decision_candidates(t: &Turn, up: &str, out: &mut Vec<Candidate>) {
    let before = out.len();
    let mut sentence_change = false;
    for sent in split_sentences(up) {
        let n = sent.chars().count();
        // a sentence that ends in a colon introduces what follows and states nothing itself:
        // `Por condición de 540 registros:` was captured from this project's own store
        if !(6..=300).contains(&n) || sent.contains('?') || sent.trim_end().ends_with(':') {
            continue;
        }
        let change = announces_change(sent);
        sentence_change |= change && !name_tokens(sent).is_empty();
        // A sentence that states a measured value and nothing else ("bump to 7 attempts")
        // announces a decision in a vocabulary no verb list covers; whether it *replaces*
        // one is decided against the store, in `supersede_quantity`.
        let bare_quantity =
            !quantity_slots(sent).is_empty() && words_outside_quantities(sent).len() <= 1;
        if !(change || bare_quantity || states_decision(sent)) {
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
            ack: if change {
                t.assistant_text.clone()
            } else {
                String::new()
            },
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
        let msg_change = announces_change(up);
        if msg_change && (!names.is_empty() || is_withdrawal(up)) {
            let words = topic_words(up);
            let object = truncate_chars(up, 300).to_string();
            out.truncate(before);
            out.push(Candidate {
                kind: "decision",
                ack: t.assistant_text.clone(),
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
    // v22: the shape that is left. The user states the new value with a reason and no verb
    // this file recognises — "argon2id is better, protects against both GPU and side-channel
    // attacks", "Tokio is the de facto standard and ecosystem support is huge" — and shares
    // no content word with what it replaces. Nothing was captured, so nothing was retired,
    // so the earlier decision stayed active and was served: three scenarios failed almost
    // completely and every one of those failures wrote the retired value into the file.
    //
    // The evidence is in the same turn and it is explicit. The assistant answers "which
    // replaces the earlier LFU eviction decision", "argon2id replaces the earlier bcrypt
    // decision (#9)". `ack_replacement` is the function that reads those, and it already
    // carries the guard this needs: it only returns a pair whose *arrival* side is part of
    // the text passed as the decision, which is what ties the reply to this message rather
    // than to something else the assistant mentioned. So the test is the function itself.
    //
    // This is the narrowest form of "the reply says it was a decision" that has evidence
    // behind it. The broader form — a bare value, with the reply saying nothing — was built
    // and thrown away after v21, because there the assistant read no decision either.
    //
    // The eight-character floor the block above uses is not this block's floor. That one keeps
    // a message with no evidence in it from becoming a decision on its own words, and eight
    // characters is a reasonable place to stop reading a sentence. Here the evidence is the
    // reply, not the message, and a message of four characters is exactly the case this loses:
    // `zstd`, `cbor`, `gzip`, `tokio`, `semver` — a person types the value and the assistant
    // answers "which supersedes the earlier gzip note on record". Nothing was typed for any of
    // them, so nothing was retired, and the fixture built out of such pairs is the one
    // condition this engine loses. The floor here is two, and the test remains the function.
    if out.len() == before
        && (2..=700).contains(&n)
        && !up.trim_end().ends_with('?')
        && ack_states_replacement(&t.assistant_text, up)
    {
        let words = topic_words(up);
        if !words.is_empty() {
            let object = truncate_chars(up, 300).to_string();
            out.push(Candidate {
                kind: "decision",
                ack: t.assistant_text.clone(),
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
                ack: String::new(),
                subject: format!("commit:{short}"),
                relation: "is".into(),
                object: redact(truncate_chars(msg, 160)),
                body: redact(&format!("commit {short} on {branch}: {msg}\n{files}")),
                origin: "commit_linked",
                anchor_path: crate::sole_anchor(&t.files_touched),
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
                ack: String::new(),
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

    /// Two Spanish decisions about different things, sharing only the filler both sentences
    /// happen to end on. Before the Spanish half of `STOP` was completed, `tiene` and
    /// `rendimiento` were the two content words `replaces` asks for, and the later decision
    /// retired the earlier one — taking with it the only record that held `Supavisor`.
    /// The user states the new value with a reason and no verb this file recognises, and the
    /// assistant answers by naming the pair outright. Three scenarios of v22 are that shape
    /// and all three failed almost completely, every failure writing the retired value into
    /// the file — which is the one thing this engine exists not to do.
    #[test]
    fn a_reply_that_names_the_pair_makes_the_message_a_decision() {
        let cases = [
            (
                "Tokio is the de facto standard and ecosystem support is huge",
                "Understood, we'll use Tokio as the async runtime, which replaces the earlier \
                 async-std decision (#20) given its ecosystem support.",
                "async-std",
            ),
            (
                "argon2id is better, protects against both GPU and side-channel attacks",
                "Understood: argon2id replaces the earlier bcrypt decision (#9) for password \
                 hashing.",
                "bcrypt",
            ),
            (
                "LRU with a 300-second TTL is cleaner and way easier to reason about",
                "Noted — LRU with a 300-second TTL, which replaces the earlier LFU eviction \
                 decision.",
                "lfu",
            ),
            // The message is the value and nothing else, which the eight-character floor on
            // the block above threw away, and the reply names what went in the noun an
            // assistant actually reaches for. Four characters, and the whole of the evidence
            // is in the reply.
            (
                "zstd",
                "Noted: zstd it is, which supersedes the earlier gzip note on record.",
                "gzip",
            ),
            (
                "cbor",
                "Got it: cbor, replacing the earlier msgpack entry.",
                "msgpack",
            ),
            // and the value it names is a phrase, so the assistant quoted it
            (
                "argon2id",
                "Noted: argon2id for password hashing, which supersedes the earlier \
                 \"We're using bcrypt\" note on record.",
                "bcrypt",
            ),
            // Both values named and the verb carrying the direction, with no `the earlier`
            // in it. Requiring that word lost this shape entirely, and it is the one an
            // assistant writes when the user's sentence is a comparison rather than a change.
            (
                "tokio fits better with the broader ecosystem.",
                "Noted: tokio replaces async-std as the async runtime, since it fits better \
                 with the broader ecosystem.",
                "async-std",
            ),
            (
                "Apache-2.0 for the license",
                "Understood: Apache-2.0 supersedes GPL-3.0 for this project.",
                "gpl-3.0",
            ),
        ];
        for (up, reply, gone) in cases {
            assert_eq!(ack_replacement(reply, up).as_deref(), Some(gone), "{up}");
            let mut t = turn(0, up);
            t.assistant_text = reply.to_string();
            let s = Session {
                turns: vec![t],
                ..Default::default()
            };
            let c = extract(&s, "abcdef12");
            let d = c
                .iter()
                .find(|x| x.kind == "decision" && x.relation == "user_decision")
                .unwrap_or_else(|| panic!("no decision for {up}"));
            assert!(d.subject.starts_with("said:change:"), "{}", d.subject);
        }
        // a reply that names no pair leaves the message where it was
        let mut t = turn(
            0,
            "LRU with a 300-second TTL is cleaner and way easier to reason about",
        );
        t.assistant_text = "Interesting, that is a common choice for hot sets.".into();
        let s = Session {
            turns: vec![t],
            ..Default::default()
        };
        assert!(!extract(&s, "abcdef12")
            .iter()
            .any(|x| x.kind == "decision" && x.relation == "user_decision"));
    }

    /// `switch` with nothing after it was not a change marker, because a bare one is as often
    /// a noun. A first person proposing one is not: "Benchmarks show zstd is faster - let's
    /// switch" ends on it, and until this it announced nothing, so no decision was captured
    /// from the message and the reply that named the pair outright was never consulted.
    #[test]
    fn a_first_person_proposing_to_switch_announces_a_change() {
        for s in [
            "Benchmarks show zstd is faster - let's switch",
            "lets switch",
            "we'll switch",
            "I'll switch",
            "time to switch",
        ] {
            assert!(announces_change(s), "{s}");
        }
        // the noun keeps its meaning, and a denial still denies
        for s in [
            "the switch is in the config",
            "add a switch for verbose output",
            "let's not switch",
        ] {
            assert!(!announces_change(s), "{s}");
        }
    }

    /// The assistant names the pair and puts an aside between the two halves of it:
    /// "switching from gzip (the earlier decision, #3) to zstd". The pattern wanted
    /// whitespace there and saw nothing, and those cells' own messages — "Benchmarks show
    /// zstd is faster - let's switch" — share no content word with what they replace, so the
    /// ack was the only thing that could pair them.
    #[test]
    fn an_ack_names_the_pair_across_a_parenthetical() {
        for (ack, new_obj, gone) in [
            (
                "Noted, switching from gzip (the earlier decision, #3) to zstd based on the benchmarks.",
                "Benchmarks show zstd is faster - let's switch",
                "gzip",
            ),
            (
                "Noted: switching versioning from calver (decision #23) to semver.",
                "semver is cleaner",
                "calver",
            ),
            (
                "Understood: we're switching from gzip (the earlier decision on record) to zstd.",
                "Benchmarks show zstd is faster",
                "gzip",
            ),
            // the shape without an aside still reads
            (
                "Got it — switching the TLS backend from openssl to rustls.",
                "rustls for TLS",
                "openssl",
            ),
            // the value behind a filler noun: `decision` is a stop word and `usable`
            // rejects it, so capturing it loses the pair entirely
            (
                "Noted, we'll go with rustls for TLS, which replaces the earlier decision \
                 to stick with openssl.",
                "Rustls makes sense for our Rust codebase, fewer CVEs and simpler",
                "openssl",
            ),
            (
                "Noted, we'll go with rustls for TLS, which supersedes the earlier choice \
                 of OpenSSL.",
                "Rustls makes sense for our Rust codebase",
                "openssl",
            ),
        ] {
            assert_eq!(ack_replacement(ack, new_obj).as_deref(), Some(gone), "{ack}");
        }
        // The same shape with a verb phrase where the value would be. There is no value in
        // "the earlier plan to turn it off in dev builds", and what came back was `it` — a
        // whole word of "Switching over to cbor, better type safety and it handles our schema
        // better", which is how the certificate decision retired the serialization one on five
        // of five cells of that fixture's wire-format task.
        assert_eq!(
            ack_replacement(
                "Understood: certificate verification stays on in all builds, dev included, \
                 which replaces the earlier plan to turn it off in dev builds.",
                "Verification on all builds is the right call, even in development environments"
            ),
            None
        );
        // The reply says a replacement happened and names what went in a clause rather than a
        // token. There is nothing there to retire a record with, and no doubt that it was
        // said, so the typing gate reads it and the retirement gate does not.
        let clause = "Understood: HTTPS everywhere with no plaintext exceptions, which \
                      replaces the earlier note that plain http was fine for internal hosts.";
        assert_eq!(
            ack_replacement(clause, "Https everywhere, no plaintext exceptions here"),
            None
        );
        assert!(super::ack_states_replacement(
            clause,
            "Https everywhere, no plaintext exceptions here"
        ));
        // …and it is still tied to this decision: a reply about something else does not count
        assert!(!super::ack_states_replacement(
            clause,
            "we should document the release process"
        ));
        // an aside is not a licence to pair anything: the `to` side must still be the
        // decision's own value
        assert_eq!(
            ack_replacement(
                "switching from gzip (the earlier decision) to zstd",
                "we should document the release process"
            ),
            None
        );
    }

    /// A prompt that specifies data to be generated states rules about that data. This
    /// store held five of them as project invariants, delivered at every session start:
    /// "The replacement must not contain the original as a substring", "In each pair the
    /// two messages are about DIFFERENT things…". Re-capturing all 1 919 transcripts of
    /// this project goes from 29 invariants to 24, and the five that go are exactly those.
    #[test]
    fn a_prompt_that_specifies_generated_data_states_no_project_rule() {
        let spec = "Invent 10 technical decisions a software team might record, each one later \
             replaced by a different choice. Each must be a value that would literally appear \
             inside a file in the repository (a dependency name, a tool name, a format, a \
             service, a number with its unit), not an abstract policy. Two hard rules: the \
             replacement must not contain the original as a substring, and neither may be a \
             word this project already uses anywhere in its own tracked files.";
        assert!(asks_for_generated_text(spec));
        let t = turn(0, spec);
        let s = Session {
            turns: vec![t],
            ..Default::default()
        };
        assert!(!extract(&s, "abcdef12")
            .iter()
            .any(|c| c.kind == "invariant"));

        // each condition alone is ordinary and takes nothing away
        assert!(!asks_for_generated_text(
            "Write 3 tests for the parser. They must never touch the network."
        ));
        let long_rule = format!(
            "We keep hitting this so I am writing it down once. {} The build must never \
             depend on the network.",
            "Context that makes this message long enough to be a specification. ".repeat(6)
        );
        assert!(!asks_for_generated_text(&long_rule));
        let long_count = format!(
            "{} We run 30 cells per arm and the seed must always be fixed before the first \
             cell runs.",
            "Some background on the grid we are about to run. ".repeat(8)
        );
        assert!(!asks_for_generated_text(&long_count));
    }

    /// A transcript pasted into a message carries that conversation's rules, not this
    /// project's. Four of this store's invariants were lines of a scaffold's JSON contract
    /// inside a PM-Bench payload whose blocks all start with `USER:`.
    #[test]
    fn a_pasted_conversation_states_no_project_rule() {
        let pasted = "USER:\nRegular tasks for every day:\n- Take the medication at 11:00.\n\n\
             USER:\nTODO Ledger (compact JSON array; edit in-place):\n\
             When action=choose, channel must be NONE and task_ids must be [].\n";
        assert!(is_pasted_conversation(pasted));
        let t = turn(0, pasted);
        let s = Session {
            turns: vec![t],
            ..Default::default()
        };
        assert!(!extract(&s, "abcdef12")
            .iter()
            .any(|c| c.kind == "invariant"));
        // one label is a person writing about a role, not a paste
        assert!(!is_pasted_conversation(
            "the user: whoever opens the page. The rule must always hold."
        ));
    }

    #[test]
    fn a_shared_spanish_filler_phrase_is_not_a_topic() {
        let pool = "mejor usamos Supavisor, tiene mejor rendimiento";
        let broker = "mejor Redpanda, tiene mejor rendimiento";
        assert!(!topic_words(pool).contains(&"tiene".to_string()));
        assert!(!replaces_text(pool, broker, true));
        // the same two decisions when they really are about the same thing still pair
        assert!(replaces_text(
            "usamos PgBouncer para el pool de conexiones",
            "mejor Supavisor para el pool de conexiones",
            true
        ));
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
        // a second file and the commit is about neither of them in particular: the anchor
        // would otherwise be whichever the harness or `git log` happened to list first
        let mut s_many = s.clone();
        s_many.turns[0].files_touched.push("docs/y.md".into());
        let cm = extract(&s_many, "abcdef12");
        let dm = cm.iter().find(|x| x.kind == "decision").unwrap();
        assert_eq!(dm.anchor_path, None);
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

#[cfg(test)]
mod split_tests {
    use super::split_sentences;

    /// An abbreviation's full stop is not a sentence boundary. This project's own store holds
    /// `CHECKOUT debe llamarse igual (p` as the whole of a record — cut at the `p.` of
    /// `p. ej.` — which is a record that says nothing and a catalogue line spent on it.
    #[test]
    fn an_abbreviation_does_not_end_a_sentence() {
        let s = split_sentences("CHECKOUT debe llamarse igual (p. ej. `gin`) en seed y en tarea");
        assert_eq!(s.len(), 1, "{s:?}");
        assert!(s[0].contains("en tarea"), "{s:?}");

        let e = split_sentences("Use tokio, e.g. for the runtime, and keep it");
        assert_eq!(e.len(), 1, "{e:?}");

        // and a real boundary still is one: the next word is capitalised
        let r = split_sentences("No, así no. Nunca uses pkill en bash.");
        assert_eq!(r.len(), 2, "{r:?}");
        assert!(r[1].starts_with("Nunca"), "{r:?}");

        // a stop after a long word is a boundary whatever follows it
        let l = split_sentences("we switched to rustls. openssl is gone");
        assert_eq!(l.len(), 2, "{l:?}");

        // and a stop inside an unclosed bracket keeps the tail with its own sentence
        let b = split_sentences("the oracle (see docs/spec.md. it is hidden) fires on merge");
        assert_eq!(b.len(), 1, "{b:?}");
        assert!(b[0].starts_with("the oracle"), "{b:?}");
    }
}

#[cfg(test)]
mod ack_tests {
    use super::ack_replacement;

    /// `[Z5]`: 23 of 30 held-out replacements share no content word with what they replace, so
    /// no lexical test pairs them, and on the plain head-to-head 14 of Muninn's 15 failing
    /// cells were a stale value served as current. The assistant's reply in the same turn
    /// sometimes names both, and how often depends on what it was shown — measured on 1 035
    /// replies recorded by earlier grids, written before this existed:
    ///
    ///     with Muninn injecting the earlier decision   168 of 630   27%
    ///     with claude-mem in the session                37 of 360   10%
    ///     with no memory at all (loop 12, 45 pairs)      0 of  45    0%
    ///
    /// So the mechanism is enabled by delivery: an assistant names the value it is replacing
    /// when something put that value in front of it. That is the failing case exactly — the
    /// stale record was served — and it is why the reach is not a property of models in
    /// general, and why it is worth having.
    #[test]
    fn an_acknowledgement_that_names_both_values_says_which_one_went() {
        let f = |ack: &str, new: &str| ack_replacement(ack, new);
        assert_eq!(
            f(
                "Got it, switching the TLS backend from openssl to rustls.",
                "Let's use rustls instead"
            ),
            Some("openssl".into())
        );
        assert_eq!(
            f(
                "Got it, switching to argon2id for password hashing instead of bcrypt.",
                "Going with argon2id for better security"
            ),
            Some("bcrypt".into())
        );
        assert_eq!(
            f(
                "Vale, cambiamos de gzip a zstd para la compresión.",
                "Cambiamos a zstd"
            ),
            Some("gzip".into())
        );
    }

    /// The shapes that say nothing are the majority, and they must stay silent rather than
    /// guess: a retirement nobody stated is the worst thing this can do.
    #[test]
    fn an_acknowledgement_that_names_only_the_new_value_says_nothing() {
        assert_eq!(
            ack_replacement(
                "Got it, switching to tokio.",
                "Actually tokio has a better ecosystem"
            ),
            None
        );
        assert_eq!(
            ack_replacement(
                "Got it, using HTTPS everywhere with no plaintext exceptions.",
                "https everywhere"
            ),
            None
        );
        assert_eq!(ack_replacement("Noted.", "Let's use rustls instead"), None);
    }

    /// And a pair about something else is not this decision's pair: the replacement side has to
    /// be what the user actually chose, or an assistant mentioning an unrelated migration in
    /// passing would retire a record nobody touched.
    #[test]
    fn a_pair_about_another_change_is_not_this_ones() {
        assert_eq!(
            ack_replacement(
                "Got it — rustls it is. Unrelated: we moved from webpack to vite last week.",
                "Let's use rustls instead"
            ),
            None
        );
    }
}

#[cfg(test)]
mod ahora_tests {
    use super::{decision_candidates, Candidate};
    use crate::model::Turn;

    fn decisions(msg: &str) -> Vec<Candidate> {
        let t = Turn {
            index: 0,
            user_prompt: msg.into(),
            ..Default::default()
        };
        let mut out = Vec::new();
        decision_candidates(&t, msg, &mut out);
        out
    }

    /// `ahora` is one of the commonest words in Spanish and it was a change marker on its own.
    /// This project's own store paid for it: a 36 MB transcript of ninety-one turns produced
    /// exactly one decision — `"Ahora dime una cosa"`, the opening of a question — keyed
    /// `said:change:` over forty topic words, and because a change supersedes, it retired two
    /// episodes on its way in.
    #[test]
    fn ahora_alone_does_not_announce_a_change() {
        let d = decisions(
            "Ahora dime una cosa. Que pasaria en un escenario donde alguien instala el plugin \
             y no quiere poner instrucciones en su CLAUDE.md",
        );
        assert!(d.is_empty(), "a question is not a decision: {d:?}");
    }

    /// And `ahora` introducing a state still is one, which is what it was in the list for.
    #[test]
    fn ahora_introducing_a_state_still_announces_one() {
        let d = decisions("Ahora usamos zstd para la compresión de transporte");
        assert!(
            d.iter().any(|c| c.subject.starts_with("said:change:")),
            "a stated change is still captured: {d:?}"
        );
    }
}

#[cfg(test)]
mod paste_tests {
    use super::{decision_candidates, unquoted, Candidate};
    use crate::model::Turn;

    fn decisions(msg: &str) -> Vec<Candidate> {
        let t = Turn {
            index: 0,
            user_prompt: msg.into(),
            ..Default::default()
        };
        let mut out = Vec::new();
        let own = unquoted(msg);
        decision_candidates(&t, own.trim(), &mut out);
        out
    }

    /// A coding session is full of pasted material, and its sentences are not things the user
    /// decided. Five of this project's own transcripts produced fourteen decisions and about
    /// ten were pasted text — one of them a line of **claude-mem's own output**, stored as a
    /// decision of this project.
    #[test]
    fn what_the_user_pasted_is_not_what_the_user_decided() {
        let fenced = "here is what the other tool printed:\n```\n54 6:43p ⚖ Transport compression codec changed to zstd\n```\nwhat do you make of it";
        assert!(decisions(fenced).is_empty(), "{:?}", decisions(fenced));
        let quoted = "> we switched from gzip to zstd last week\nis that still true";
        assert!(decisions(quoted).is_empty(), "{:?}", decisions(quoted));

        // and the user's own sentence around a paste still counts
        let mixed = "let's switch to zstd for compression\n```\nsome log output\n```";
        assert!(
            !decisions(mixed).is_empty(),
            "the user's own words survive: {:?}",
            decisions(mixed)
        );
    }

    /// `No cambié nada` is the denial of a change and it became a `said:change:` record — the
    /// kind that supersedes. Found in this project's own store.
    #[test]
    fn a_denied_change_is_not_a_change() {
        assert!(decisions("No cambié nada").is_empty());
        assert!(decisions("I didn't switch to anything").is_empty());
        assert!(
            !decisions("Cambiamos a zstd para la compresión").is_empty(),
            "a stated change still is one"
        );
    }

    /// A sentence ending in a colon introduces what follows and states nothing itself.
    #[test]
    fn a_header_is_not_a_decision() {
        assert!(decisions("Por condición de 540 registros:").is_empty());
    }
}

#[cfg(test)]
mod correction_tests {
    use super::is_correction;

    /// "I don't understand" opens with the same `no` the correction markers look for. On this
    /// project's 240 real user messages three match the opener and **two of them are this** —
    /// a request to explain, recorded as "you were corrected here".
    #[test]
    fn asking_what_you_meant_is_not_a_correction() {
        assert!(!is_correction(
            "No entendi muy bien lo que me dijiste de la sesion"
        ));
        assert!(!is_correction("No entiendo qué cambió"));
        assert!(!is_correction("I don't understand what you changed"));

        // and a real correction still is one
        assert!(is_correction("No hagas nada, solo responde"));
        assert!(is_correction("No, así no. Usa rustls"));
        assert!(is_correction(
            "Veo que dice que treesitter sera parte de la version 1.1 a pesar de que te dije que tiene que ser parte del MVP"
        ));
    }
}
