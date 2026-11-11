//! Deterministic text clean-up applied to every transcript (also the fallback when the LLM step is
//! off or fails): spoken commands, filler removal, dictionary fixes, spoken symbols, snippet
//! expansion, spacing and tone-specific capitalization/punctuation.

use crate::db::{DictEntry, Snippet};
use crate::learn::common;
use crate::terms::{norm, standalone};
use regex::{Captures, Regex, RegexBuilder};
use std::collections::HashMap;
use std::sync::LazyLock;

pub struct Options<'a> {
    pub remove_fillers: bool,
    pub voice_commands: bool,
    pub dictionary: &'a [DictEntry],
    pub snippets: &'a [Snippet],
    pub tone: &'a str,
    /// Dictation language is English: "um"/"er" are fillers. In other languages they are real
    /// words ("um 3 Uhr", "Er kommt", "tenho um carro"), so they're only removed before a comma.
    pub english: bool,
    /// Terminal / code editor / AI prompt: keep text verbatim (no capitalization, no tone).
    pub technical: bool,
    /// Code editor, terminal or AI prompt: spoken symbols between identifiers are code
    /// ("user underscore id" -> user_id). Elsewhere only unambiguous file and domain names join.
    pub code: bool,
    /// AI prompt in a text field: wrap file names, paths, flags and identifiers in backticks.
    pub backticks: bool,
    /// Fix mis-hearings of built-in tech terms (`terms.rs`).
    pub terms: bool,
    /// An accepted auto action owns layout, including code indentation and nested lists.
    pub preserve_formatting: bool,
    /// Reads local text only when a matched snippet actually expands `{clipboard}`.
    pub clipboard: Option<&'a (dyn Fn() -> Option<String> + Sync)>,
    /// The workspace in use (technical contexts and AI prompts only), for the two passes below.
    pub workspace: Option<&'a crate::workspace::Index>,
    /// The workspace's names said as words are written as code ("user session store" ->
    /// UserSessionStore). Opt-in, editors and terminals only: it can rewrite ordinary phrases.
    pub workspace_names: bool,
    /// AI prompts: "at sign main.rs" or "@main.rs" becomes "@" and the workspace file's path.
    pub mentions: bool,
}

impl Options<'_> {
    pub fn expand_snippet(&self, expansion: &str) -> String {
        let clipboard = if expansion.contains("{clipboard}") { self.clipboard.and_then(|read| read()) } else { None };
        expand_vars(expansion, clipboard.as_deref())
    }
}

#[derive(Debug, Default, PartialEq)]
pub struct Processed {
    pub text: String,
    /// Dictionary corrections + removed fillers (shown as "fixes" in Insights).
    pub fixes: usize,
    /// User ended with "press enter" — send Enter after inserting.
    pub press_enter: bool,
    /// Dictionary entries that matched (usage ranks vocabulary hints).
    pub used: Vec<i64>,
}

/// Fillers that aren't words in any supported language.
static FILLERS: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)(^|[\s,.])(?:u+m{2,}|u+h+m*|e+r+m+|h+m+|m{2,})[,.]?(\s+|$)").unwrap());
/// "um"/"er": fillers in English, words elsewhere — then only when set off by a comma.
static FILLERS_EN: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)(^|[\s,.])(?:u+m|e+r)[,.]?(\s+|$)").unwrap());
static FILLERS_PAUSE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)(^|[\s,.])(?:u+m|e+r),(\s+|$)").unwrap());
static NEW_PARAGRAPH: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)[,.;]?\s*\bnew paragraph\b[,.;:]?\s*").unwrap());
static NEW_LINE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)[,.;]?\s*\bnew line\b[,.;:]?\s*").unwrap());
static PRESS_ENTER: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)[,.;]?\s*\b(?:press|hit) enter\b[.!]?\s*$").unwrap());
static SCRATCH: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)[,.;]?\s*\bscratch that\b[,.;!]?\s*").unwrap());
/// "word ." -> "word." but not ".NET" or "./run.sh" (punctuation followed by a word character).
static SPACE_BEFORE_PUNCT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[ \t]+([,.;:!?]+)(\s|$)").unwrap());
static MULTI_SPACE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[ \t]{2,}").unwrap());
static SPACE_AROUND_NL: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[ \t]*\n[ \t]*").unwrap());
static DOUBLE_PUNCT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"([,;:])\s*([.!?])").unwrap());

pub fn process(raw: &str, o: &Options) -> Processed {
    let mut fixes = 0;
    let mut text = raw.trim().to_string();

    let mut press_enter = false;
    if o.voice_commands {
        if PRESS_ENTER.is_match(&text) {
            press_enter = true;
            text = PRESS_ENTER.replace(&text, "").into_owned();
        }
        text = apply_scratch_that(&text);
        text = NEW_PARAGRAPH.replace_all(&text, "\n\n").into_owned();
        text = NEW_LINE.replace_all(&text, "\n").into_owned();
        text = SPOKEN_QUOTE.replace_all(&text, "\"$1\"").into_owned();
        text = SPOKEN_BACKTICK.replace_all(&text, "`$1`").into_owned();
    }

    if o.remove_fillers {
        // Repeat because adjacent fillers share the separating whitespace.
        let ambiguous: &Regex = if o.english { &FILLERS_EN } else { &FILLERS_PAUSE };
        for re in [&*FILLERS, ambiguous] {
        loop {
            let mut removed = 0;
            let next = re
                .replace_all(&text, |c: &regex::Captures| {
                    let word = c[0].trim_matches(|ch: char| !ch.is_alphabetic());
                    // Keep acronyms such as "ERM" or "HMM".
                    if word.len() > 2 && word.chars().all(|ch| ch.is_uppercase()) {
                        c[0].to_string()
                    } else {
                        removed += 1;
                        c[1].to_string()
                    }
                })
                .into_owned();
            if removed == 0 {
                break;
            }
            fixes += removed;
            text = next;
        }
        }
    }

    // One pass, longest phrase first; then the entries' own spellings are restored at the end,
    // after capitalization and tone (which would otherwise turn "kubectl" into "Kubectl").
    let fix = Dictionary::new(o.dictionary, true);
    let spell = Dictionary::new(o.dictionary, false);
    let mut used = Vec::new();
    if let Some(d) = &fix {
        let (fixed, n) = d.apply(&text, Some(&mut used));
        text = fixed;
        fixes += n;
    }
    // Text the user's dictionary or snippets own: later rules must leave it alone.
    let triggers: Vec<Regex> = o.snippets.iter().filter_map(|s| phrase_regex(&s.trigger)).collect();
    let owned = |text: &str| {
        let matchers = fix.iter().map(|d| &d.re).chain(&triggers);
        matchers.flat_map(|re| re.find_iter(text).map(|m| (m.start(), m.end()))).collect::<Vec<_>>()
    };
    if o.voice_commands {
        text = spoken_symbols(&text, o.code, &owned);
    }
    // Before the term packs, so a longer repository name wins over a pack alias inside it.
    if let Some(w) = o.workspace {
        if o.workspace_names {
            let (fixed, n) = w.fix(&text, &owned(&text));
            text = fixed;
            fixes += n;
        }
        if o.mentions {
            text = w.mentions(&text);
        }
    }
    if o.terms {
        let (fixed, n) = crate::terms::fix(&text, o.technical, &owned(&text));
        text = fixed;
        fixes += n;
    }
    if o.backticks {
        text = wrap_code(&text);
    }

    // A dictation that is just a snippet trigger becomes exactly the expansion.
    for s in o.snippets {
        if normalize(&text) == normalize(&s.trigger) {
            return Processed { text: o.expand_snippet(&s.expansion), fixes, press_enter, used };
        }
    }
    for s in o.snippets {
        if let Some(re) = phrase_regex(&s.trigger) {
            if re.is_match(&text) {
                text = re.replace_all(&text, regex::NoExpand(&o.expand_snippet(&s.expansion))).into_owned();
            }
        }
    }

    if !o.preserve_formatting {
        text = tidy(&text, !o.technical);
        if !o.technical {
            text = apply_tone(&text, o.tone);
        }
    }
    if let Some(d) = &spell {
        text = d.apply(&text, None).0;
    }
    Processed { text, fixes, press_enter, used }
}

/// The user's dictionary as one case-insensitive matcher. `fix` matches each entry's phrase and
/// its output (so a transcriber's "Kubectl" becomes "kubectl" too); otherwise only the outputs, to
/// restore their spelling. An output that is a common word ("Go") is only produced, never matched:
/// "let's go" stays as said. Longest first, in one pass, so overlapping entries ("react" and
/// "react native") don't depend on their order and no entry rewrites another's output.
struct Dictionary<'a> {
    re: Regex,
    /// Normalized match -> entry (a phrase wins over another entry's output).
    entries: HashMap<String, &'a DictEntry>,
}

impl<'a> Dictionary<'a> {
    fn new(dictionary: &'a [DictEntry], fix: bool) -> Option<Self> {
        let owns_output = |e: &&DictEntry| e.replacement.is_none() || !common(output(e));
        let phrases = dictionary.iter().filter(|_| fix).map(|e| (e.phrase.as_str(), e));
        let outputs = dictionary.iter().filter(owns_output).map(|e| (output(e), e));
        let mut entries = HashMap::new();
        let mut patterns = Vec::new();
        for (p, e) in phrases.chain(outputs) {
            if let (Some(pattern), std::collections::hash_map::Entry::Vacant(v)) = (phrase_pattern(p), entries.entry(norm(p))) {
                v.insert(e);
                patterns.push(pattern);
            }
        }
        if patterns.is_empty() {
            return None;
        }
        patterns.sort_by_key(|p| std::cmp::Reverse(p.len()));
        let alternation = patterns.iter().map(|p| format!("(?:{p})")).collect::<Vec<_>>().join("|");
        match RegexBuilder::new(&alternation).case_insensitive(true).size_limit(1 << 25).build() {
            Ok(re) => Some(Dictionary { re, entries }),
            Err(e) => {
                log::warn!("dictionary not applied: {e}");
                None
            }
        }
    }

    /// Rewrites each match to its entry's output, except where it is part of a path, URL or file
    /// name. Returns the text and how many matches changed; `used` collects the matched entries.
    fn apply(&self, text: &str, mut used: Option<&mut Vec<i64>>) -> (String, usize) {
        let mut changed = 0;
        let out = self.re.replace_all(text, |c: &Captures| {
            let m = c.get(0).unwrap();
            let Some(e) = self.entries.get(&norm(m.as_str())).filter(|_| standalone(text, m.start(), m.end())) else {
                return m.as_str().to_string();
            };
            if let (Some(used), Some(id)) = (used.as_deref_mut(), e.id) {
                if !used.contains(&id) {
                    used.push(id);
                }
            }
            changed += usize::from(m.as_str() != output(e));
            output(e).to_string()
        });
        (out.into_owned(), changed)
    }
}

fn output(e: &DictEntry) -> &str {
    e.replacement.as_deref().unwrap_or(&e.phrase)
}

/// Applies only the user's dictionary (meeting transcripts, spoken commands).
pub fn apply_dictionary(text: &str, dictionary: &[DictEntry]) -> String {
    Dictionary::new(dictionary, true).map_or_else(|| text.to_string(), |d| d.apply(text, None).0)
}

/// Whether these entries build into one matcher (it fails past a size limit, and then none applies).
pub fn dictionary_builds(dictionary: &[DictEntry]) -> bool {
    Dictionary::new(dictionary, true).is_some() || dictionary.iter().all(|e| phrase_pattern(&e.phrase).is_none())
}

/// "quote hello world end quote" -> "hello world" in double quotes (also "unquote", "close quote").
static SPOKEN_QUOTE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\b(?:open[ \t]+)?quote[,.:]?[ \t]+([^\n]+?)[,.]?[ \t]+(?:(?:end|close)[ \t]+quote|unquote)\b").unwrap()
});
/// "backtick npm install backtick" -> `npm install` (also "end backtick", "close backtick").
static SPOKEN_BACKTICK: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\b(?:open[ \t]+)?back[ \t-]?tick[,.:]?[ \t]+([^\n]+?)[,.]?[ \t]+(?:(?:end|close)[ \t]+)?back[ \t-]?tick\b").unwrap()
});
/// "<identifier> dot|slash|backslash|underscore <word>"; joined pieces chain ("src/app slash main").
static SPOKEN_JOIN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)([\w~](?:[\w.~/\\-]*[\w~])?)[ \t]+(dot|slash|back[ \t]?slash|underscore)[ \t]+(\w+)").unwrap()
});
/// Spoken prefixes in code: "dot slash run" -> ./run, "tilde slash" -> ~/, "dash dash watch" -> --watch.
static SPOKEN_PREFIX: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)(^|\s)(dot[ \t]+slash|tilde[ \t]+slash|(?:dash[ \t]+dash|double[ \t]+dash))[ \t]+(\w)").unwrap()
});
/// A dotfile anywhere: "the dot env file" -> the .env file.
static SPOKEN_DOTFILE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)(^|\s)dot[ \t]+(env|gitignore|dockerignore|npmrc|nvmrc|bashrc|zshrc|editorconfig|prettierrc|eslintrc)\b").unwrap()
});
/// Extensions spoken letter by letter: "main dot r s" -> main dot rs (joined if it is one).
static SPELLED_EXTENSION: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)\bdot[ \t]+([a-z])[ \t]+([a-z])(?:[ \t]+([a-z]))?\b").unwrap());
/// A slash command at the start of an AI prompt or terminal input: "slash compact" -> /compact.
static SPOKEN_COMMAND: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)^slash[ \t]+(\w)").unwrap());

const EXTENSIONS: &[&str] = &[
    "rs", "ts", "tsx", "js", "jsx", "mjs", "cjs", "py", "rb", "go", "java", "kt", "swift", "c", "h", "cpp", "hpp", "cs", "php",
    "sh", "bash", "zsh", "ps1", "json", "yaml", "yml", "toml", "xml", "html", "css", "scss", "md", "mdx", "txt", "csv", "sql",
    "lock", "env", "log", "ini", "cfg", "conf", "vue", "svelte", "dart", "lua", "pdf", "png", "jpg", "jpeg", "svg", "gif",
    "zip", "proto", "graphql", "tf", "dockerfile",
];
const DOMAINS: &[&str] = &["com", "org", "net", "io", "dev", "ai", "co", "edu", "gov"];

fn extension(word: &str) -> bool {
    EXTENSIONS.contains(&word.to_lowercase().as_str())
}

/// Spoken symbols as characters, but only where they are clearly meant as code: "main dot rs" ->
/// main.rs and "example dot com" -> example.com everywhere; between identifiers only in code
/// contexts, and never next to common words ("the dot product", "slash prices", "to underscore
/// the point" stay as said). Text the user's dictionary or snippets own is left alone.
fn spoken_symbols(text: &str, code: bool, owned: &dyn Fn(&str) -> Vec<(usize, usize)>) -> String {
    let mut text = SPELLED_EXTENSION
        .replace_all(text, |c: &Captures| {
            let letters: String = (1..=3).filter_map(|i| c.get(i)).map(|m| m.as_str()).collect();
            if extension(&letters) { format!("dot {letters}") } else { c[0].to_string() }
        })
        .into_owned();
    text = SPOKEN_DOTFILE.replace_all(&text, "$1.$2").into_owned();
    if code {
        text = SPOKEN_COMMAND.replace(&text, "/$1").into_owned();
        text = SPOKEN_PREFIX
            .replace_all(&text, |c: &Captures| {
                let spoken = c[2].to_lowercase();
                let prefix = if spoken.starts_with("dot") { "./" } else if spoken.starts_with("tilde") { "~/" } else { "--" };
                format!("{}{prefix}{}", &c[1], &c[3])
            })
            .into_owned();
    }
    // Each round joins one symbol per identifier; chains ("src slash app slash main dot rs") take a few.
    for _ in 0..16 {
        if !SPOKEN_JOIN.is_match(&text) {
            break;
        }
        let keep = owned(&text);
        let next = SPOKEN_JOIN.replace_all(&text, |c: &Captures| {
            let whole = c.get(0).unwrap();
            let (a, symbol, b) = (&c[1], c[2].to_lowercase(), &c[3]);
            let ext = extension(b) || DOMAINS.contains(&b.to_lowercase().as_str());
            let is = |w: &str, list: &[&str]| list.contains(&w.to_lowercase().as_str());
            let article = |w: &str| is(w, &["a", "an", "the"]);
            let join = match symbol.as_str() {
                "dot" => (ext && !common(a)) || (code && ((ext && !article(a)) || (!common(a) && !common(b)))),
                // snake_case is often made of common words (get_user, to_string), but prose isn't code:
                // "to underscore the point", "join them with underscore characters".
                "underscore" => {
                    let prose = is(a, &["a", "an", "the", "with", "by", "from", "of", "in", "for", "as", "than", "into", "and", "or", "use", "using"]);
                    code && !prose && !article(b) && !(common(a) && common(b))
                }
                _ => code && !common(a) && !common(b),
            };
            if !join || keep.iter().any(|&(s, e)| whole.start() < e && s < whole.end()) {
                return whole.as_str().to_string();
            }
            let (symbol, b) = match symbol.as_str() {
                "dot" if ext => (".", b.to_lowercase()),
                "dot" => (".", b.to_string()),
                "slash" => ("/", b.to_string()),
                "underscore" => ("_", b.to_string()),
                _ => ("\\", b.to_string()),
            };
            format!("{a}{symbol}{b}")
        });
        if next == text {
            break;
        }
        text = next.into_owned();
    }
    text
}

/// AI prompts: wraps file names, paths, flags and snake_case identifiers in backticks. Leaves
/// URLs, emails, product names ("Next.js") and anything already quoted alone.
fn wrap_code(text: &str) -> String {
    let code_like = |t: &str| {
        let allowed = t.chars().all(|c| c.is_ascii_alphanumeric() || "_./~-".contains(c));
        let letter = t.chars().any(|c| c.is_ascii_alphabetic());
        let file = t.rsplit_once('.').is_some_and(|(stem, ext)| {
            stem.chars().any(|c| c.is_ascii_alphabetic()) && stem.len() + ext.len() > 2 && !stem.ends_with('/') && extension(ext)
        });
        let dotfile = t.len() > 2 && t.starts_with('.') && t[1..].chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit());
        let path = t.starts_with("./") || t.starts_with("~/") || (t.starts_with('/') && t.len() > 1);
        let flag = t.starts_with("--") && t[2..].starts_with(|c: char| c.is_ascii_alphanumeric());
        let snake = t.split('_').filter(|p| !p.is_empty()).count() > 1;
        allowed && letter && !t.starts_with("www.") && !crate::terms::is_term(t) && (file || dotfile || path || flag || snake)
    };
    let mut out = String::with_capacity(text.len() + 16);
    for piece in text.split_inclusive(char::is_whitespace) {
        let (word, space) = piece.split_at(piece.trim_end().len());
        let inner = word.trim_start_matches('(');
        let core = inner.trim_end_matches(['.', ',', ';', ':', '!', '?', ')']);
        if code_like(core) {
            out.push_str(&format!("{}`{core}`{}{space}", &word[..word.len() - inner.len()], &inner[core.len()..]));
        } else {
            out.push_str(piece);
        }
    }
    out
}

/// "I'll come at 2. Scratch that. I'll come at 3." -> "I'll come at 3."
fn apply_scratch_that(text: &str) -> String {
    let mut out = text.to_string();
    while let Some(m) = SCRATCH.find(&out) {
        let before = out[..m.start()].trim_end();
        // Drop the last sentence before the command.
        let cut = before
            .char_indices()
            .rev()
            .find(|(_, c)| matches!(c, '.' | '!' | '?' | '\n'))
            .map(|(i, c)| i + c.len_utf8())
            .unwrap_or(0);
        out = format!("{} {}", &before[..cut], &out[m.end()..]);
    }
    out
}

/// Case-insensitive whole-phrase matcher that tolerates spaces/hyphens between words.
fn phrase_regex(phrase: &str) -> Option<Regex> {
    RegexBuilder::new(&phrase_pattern(phrase)?).case_insensitive(true).build().ok()
}

/// Matches a phrase with any spacing or hyphens between its words, as a whole word.
pub fn phrase_pattern(phrase: &str) -> Option<String> {
    let words: Vec<String> = phrase.split_whitespace().map(regex::escape).collect();
    if words.is_empty() {
        return None;
    }
    let body = words.join(r"[\s\-]+");
    // `\b` only works next to word characters; "C++", "C#" or ".NET" need no boundary there.
    let word_char = |c: Option<char>| c.map(|c| c.is_alphanumeric() || c == '_').unwrap_or(false);
    let start = if word_char(phrase.trim().chars().next()) { r"\b" } else { "" };
    let end = if word_char(phrase.trim().chars().last()) { r"\b" } else { "" };
    Some(format!(r"{start}{body}{end}"))
}

/// Fills in a snippet's placeholders: `{date}` (October 15, 2026), `{time}` (14:05), `{weekday}`
/// and `{clipboard}`. Anything else in braces is left as written.
pub fn expand_vars(expansion: &str, clipboard: Option<&str>) -> String {
    expand_vars_at(expansion, clipboard, chrono::Local::now().naive_local())
}

fn expand_vars_at(expansion: &str, clipboard: Option<&str>, now: chrono::NaiveDateTime) -> String {
    if !expansion.contains('{') {
        return expansion.to_string();
    }
    expansion
        .replace("{date}", &now.format("%B %-d, %Y").to_string())
        .replace("{time}", &now.format("%H:%M").to_string())
        .replace("{weekday}", &now.format("%A").to_string())
        .replace("{clipboard}", clipboard.unwrap_or("").trim())
}

/// The snippet whose trigger is the whole dictation, if any (it becomes exactly the expansion).
pub fn whole_snippet<'a>(text: &str, snippets: &'a [Snippet]) -> Option<&'a Snippet> {
    let t = normalize(text);
    snippets.iter().find(|s| t == normalize(&s.trigger))
}

/// The snippet triggers that occur in `text`.
pub fn snippet_triggers_in(text: &str, snippets: &[Snippet]) -> Vec<String> {
    snippets.iter().filter(|s| phrase_regex(&s.trigger).is_some_and(|re| re.is_match(text))).map(|s| s.trigger.clone()).collect()
}

fn normalize(s: &str) -> String {
    s.trim()
        .trim_end_matches(['.', '!', '?', ','])
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

fn tidy(text: &str, capitalize: bool) -> String {
    let t = SPACE_BEFORE_PUNCT.replace_all(text, "$1$2");
    let t = DOUBLE_PUNCT.replace_all(&t, "$2");
    let t = MULTI_SPACE.replace_all(&t, " ");
    let t = SPACE_AROUND_NL.replace_all(&t, "\n");
    let t = t.trim_matches(|c: char| c == ' ' || c == ',' || c == ';');
    if capitalize {
        capitalize_sentences(t)
    } else {
        t.to_string()
    }
}

fn capitalize_sentences(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut cap_next = true;
    for (i, &c) in chars.iter().enumerate() {
        if cap_next && c.is_alphabetic() {
            // File names, domains and identifiers keep their case ("main.rs", "example.com").
            let word: Vec<char> = chars[i..].iter().copied().take_while(|c| !c.is_whitespace()).collect();
            if word.windows(2).any(|w| "./_".contains(w[0]) && w[1].is_alphanumeric()) {
                out.push(c);
            } else {
                out.extend(c.to_uppercase());
            }
            cap_next = false;
        } else {
            out.push(c);
            if ends_sentence(&chars, i) {
                cap_next = true;
            } else if !c.is_whitespace() && !matches!(c, '"' | '\'' | '(' | '-' | '*') {
                cap_next = false;
            }
        }
    }
    out
}

/// Sentence end = newline, or . ! ? followed by whitespace (not "example.com" or "3.5").
fn ends_sentence(chars: &[char], i: usize) -> bool {
    match chars[i] {
        '\n' => true,
        '.' | '!' | '?' => chars.get(i + 1).map(|n| n.is_whitespace()).unwrap_or(false),
        _ => false,
    }
}

/// Style only touches capitalization and terminal punctuation, never wording.
pub fn apply_tone(text: &str, tone: &str) -> String {
    if text.is_empty() {
        return String::new();
    }
    let ends_with_punct = text.ends_with(['.', '!', '?', ':', ')', '"']);
    match tone {
        "none" => text.to_string(),
        "very_casual" => {
            let mut t = lowercase_sentence_starts(text);
            if t.ends_with('.') && !t.ends_with("..") {
                t.pop();
            }
            t
        }
        "casual" => {
            let mut t = text.to_string();
            // Drop a lone trailing period on single-sentence messages.
            if t.ends_with('.') && !t[..t.len() - 1].contains(['.', '!', '?', '\n']) {
                t.pop();
            }
            t
        }
        "excited" => {
            let mut t = text.to_string();
            if t.ends_with('.') {
                t.pop();
                t.push('!');
            } else if !ends_with_punct {
                t.push('!');
            }
            t
        }
        _ => {
            let mut t = text.to_string();
            if !ends_with_punct && t.split_whitespace().count() > 2 && !t.ends_with('\n') {
                t.push('.');
            }
            t
        }
    }
}

fn lowercase_sentence_starts(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut at_start = true;
    let chars: Vec<char> = text.chars().collect();
    for (i, &c) in chars.iter().enumerate() {
        if at_start && c.is_alphabetic() {
            // Keep "I", acronyms and words that are capitalized throughout (names stay as spoken).
            let next = chars.get(i + 1).copied().unwrap_or(' ');
            let is_word_i = c == 'I' && !next.is_alphanumeric();
            if c.is_uppercase() && !next.is_uppercase() && !is_word_i {
                out.extend(c.to_lowercase());
            } else {
                out.push(c);
            }
            at_start = false;
        } else {
            out.push(c);
            if ends_sentence(&chars, i) {
                at_start = true;
            } else if !c.is_whitespace() {
                at_start = false;
            }
        }
    }
    out
}

pub fn word_count(text: &str) -> usize {
    text.split_whitespace().count()
}

#[cfg(test)]
mod tests {
    #[test]
    fn snippet_variables() {
        let at = chrono::NaiveDate::from_ymd_opt(2026, 10, 15).unwrap().and_hms_opt(14, 5, 0).unwrap();
        assert_eq!(
            expand_vars_at("Hi {clipboard}, as of {weekday} {date} {time}. {unknown} {}", Some(" Priya \n"), at),
            "Hi Priya, as of Thursday October 15, 2026 14:05. {unknown} {}"
        );
        assert_eq!(expand_vars_at("{clipboard}!", None, at), "!");
        let s = vec![Snippet { id: None, trigger: "sign off".into(), expansion: "Thanks, {clipboard}".into(), created_at: None }];
        let o = Options { clipboard: Some(&|| Some("Alex".into())), ..opts(&[], &s, "none") };
        assert_eq!(process("sign off", &o).text, "Thanks, Alex");
        assert_eq!(process("ok, sign off", &o).text, "Ok, Thanks, Alex");
    }

    #[test]
    fn clipboard_is_read_only_for_an_expanded_placeholder() {
        let snippets = vec![
            Snippet { id: None, trigger: "paste context".into(), expansion: "Context: {clipboard}".into(), created_at: None },
            Snippet { id: None, trigger: "sign off".into(), expansion: "Regards, Alex".into(), created_at: None },
        ];
        let reads = std::sync::atomic::AtomicUsize::new(0);
        let read = || { reads.fetch_add(1, std::sync::atomic::Ordering::SeqCst); Some("private $1 {date}".into()) };
        let o = Options { clipboard: Some(&read), ..opts(&[], &snippets, "none") };
        process("ordinary dictation", &o);
        process("sign off", &o);
        assert_eq!(reads.load(std::sync::atomic::Ordering::SeqCst), 0);
        assert_eq!(process("paste context", &o).text, "Context: private $1 {date}");
        assert_eq!(reads.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    #[test]
    fn auto_actions_keep_code_and_nested_list_indentation() {
        let dictionary = vec![DictEntry { id: Some(1), phrase: "post gress".into(), replacement: Some("Postgres".into()), created_at: None, learned: false }];
        let o = Options { remove_fillers: false, voice_commands: false, preserve_formatting: true, ..opts(&dictionary, &[], "formal") };
        let output = "- Plan\n  - Use post gress\n\n```python\nif ready:\n    run()\n```";
        assert_eq!(process(output, &o).text, output.replace("post gress", "Postgres"));
    }

    #[test]
    fn whole_snippets_and_triggers() {
        let sn = |t: &str| Snippet { id: None, trigger: t.into(), expansion: format!("<{t}>"), created_at: None };
        let snippets = vec![sn("my address"), sn("calendar link")];
        assert_eq!(whole_snippet("My address.", &snippets).map(|s| s.expansion.as_str()), Some("<my address>"));
        assert!(whole_snippet("send my address", &snippets).is_none());
        assert_eq!(snippet_triggers_in("Here's my calendar-link, thanks", &snippets), vec!["calendar link".to_string()]);
        assert!(snippet_triggers_in("nothing here", &snippets).is_empty());
    }

    use super::*;

    fn opts<'a>(d: &'a [DictEntry], s: &'a [Snippet], tone: &'a str) -> Options<'a> {
        Options { remove_fillers: true, voice_commands: true, dictionary: d, snippets: s, tone, english: true, technical: false, code: false, backticks: false, terms: false, preserve_formatting: false, clipboard: None, workspace: None, workspace_names: false, mentions: false }
    }

    #[test]
    fn built_in_terms_after_the_users_dictionary() {
        let d = [dict("cube control", Some("CubeCtl"))];
        let o = Options { terms: true, technical: true, ..opts(&d, &[], "none") };
        let p = process("cube control then post gress", &o);
        assert_eq!((p.text.as_str(), p.fixes), ("CubeCtl then Postgres", 2));
        assert_eq!(process("cube control then post gress", &Options { technical: true, ..opts(&[], &[], "none") }).text, "cube control then post gress", "off");

        // An entry counts as used when its output is there, even if the transcriber got it right.
        let d = [DictEntry { id: Some(7), ..dict("cube control", Some("kubectl")) }];
        assert_eq!(process("run kubectl", &opts(&d, &[], "none")).used, vec![7]);
        assert_eq!(process("run cube control", &opts(&d, &[], "none")).used, vec![7]);
        assert!(process("run it", &opts(&d, &[], "none")).used.is_empty());

        // A spelling-only entry that is also a built-in alias keeps the user's spelling.
        let d = [dict("Ducker", None)];
        assert_eq!(process("start ducker now", &Options { terms: true, technical: true, ..opts(&d, &[], "none") }).text, "start Ducker now");
        // A snippet trigger that is also a built-in alias still expands.
        let s = [Snippet { id: None, trigger: "type script".into(), expansion: "our TypeScript project instructions".into(), created_at: None }];
        let o = Options { terms: true, technical: true, ..opts(&[], &s, "none") };
        assert_eq!(process("type script", &o).text, "our TypeScript project instructions");
        assert_eq!(process("see type script here", &o).text, "see our TypeScript project instructions here");
    }

    #[test]
    fn workspace_names_and_file_mentions() {
        let v = |l: &[&str]| l.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let scan = crate::workspace::Scan { files: v(&["desktop/src/pipeline.rs"]), identifiers: v(&["UserSessionStore", "PostgresPoolManager"]), sources: Vec::new() };
        let ws = crate::workspace::Index::new(scan, crate::terms::SIZE_LIMIT);
        let t = |s: &str, o: &Options| process(s, o).text;
        let off = Options { workspace: Some(&ws), terms: true, mentions: true, ..code_opts(&[]) };
        assert_eq!(t("fix the user session store in at sign pipeline dot rs", &off), "fix the user session store in @desktop/src/pipeline.rs", "names: off by default");
        let o = Options { workspace: Some(&ws), workspace_names: true, terms: true, ..code_opts(&[]) };
        assert_eq!(t("fix the user session store, then at sign pipeline dot rs", &o), "fix the UserSessionStore, then at sign pipeline.rs");
        let prompt = Options { mentions: true, backticks: true, ..o };
        assert_eq!(t("fix the user session store in at sign pipeline dot rs", &prompt), "fix the UserSessionStore in @desktop/src/pipeline.rs");
        assert_eq!(t("look at pipeline dot rs", &prompt), "look at `pipeline.rs`", "only an explicit mention");
        assert_eq!(t("tune the postgres pool manager", &o), "tune the PostgresPoolManager", "before the term packs");
        assert_eq!(t("user session store", &Options { workspace: None, ..o }), "user session store");
        // The user's dictionary and snippets win.
        let d = [dict("user session store", Some("USS"))];
        assert_eq!(t("open the user session store", &Options { dictionary: &d, ..o }), "open the USS");
        let s = [Snippet { id: None, trigger: "user session store".into(), expansion: "the session cache".into(), created_at: None }];
        assert_eq!(t("open the user session store", &Options { snippets: &s, ..o }), "open the the session cache");
    }

    #[test]
    fn fillers_respect_other_languages() {
        let o = Options { english: false, ..opts(&[], &[], "formal") };
        assert_eq!(process("Er kommt um 3 Uhr.", &o).text, "Er kommt um 3 Uhr.");
        assert_eq!(process("Eu tenho um carro.", &o).text, "Eu tenho um carro.");
        assert_eq!(process("Um, ich denke, uh, ja.", &o).text, "Ich denke, ja.");
    }

    #[test]
    fn technical_context_stays_verbatim() {
        let o = Options { technical: true, ..opts(&[], &[], "formal") };
        assert_eq!(process("git commit -m fix", &o).text, "git commit -m fix");
    }

    #[test]
    fn dictionary_entries_with_symbols() {
        let d = [dict("C++", None), dict("c sharp", Some("C#")), dict(".NET", None)];
        assert_eq!(process("i write c++ and c sharp on .net", &opts(&d, &[], "casual")).text, "I write C++ and C# on .NET");
    }

    fn dict(p: &str, r: Option<&str>) -> DictEntry {
        DictEntry { id: None, phrase: p.into(), replacement: r.map(Into::into), created_at: None, learned: false }
    }

    #[test]
    fn removes_fillers() {
        let p = process("Um, so I think, uh, we should ship it. Hmm", &opts(&[], &[], "formal"));
        assert_eq!(p.text, "So I think, we should ship it.");
        assert_eq!(p.fixes, 3);
        assert_eq!(process("The ERM module and umbrella", &opts(&[], &[], "formal")).text, "The ERM module and umbrella.");
    }

    #[test]
    fn voice_commands() {
        let p = process("Hello team, new paragraph. Here is the update new line done. Press enter.", &opts(&[], &[], "formal"));
        assert_eq!(p.text, "Hello team\n\nHere is the update\nDone.");
        assert!(p.press_enter);
        let p = process("Meet at 2pm. Scratch that. Meet at 3pm.", &opts(&[], &[], "formal"));
        assert_eq!(p.text, "Meet at 3pm.");
    }

    #[test]
    fn dictionary_replacements_and_spelling() {
        let d = [dict("nest dev", Some("feature/nest_dev")), dict("Visual Studio", None), dict("Kubernetes", None)];
        let p = process("merge nest-dev into main and tell visual studio about kubernetes", &opts(&d, &[], "casual"));
        assert_eq!(p.text, "Merge feature/nest_dev into main and tell Visual Studio about Kubernetes");
        assert_eq!(p.fixes, 3);
        // Must not match inside other words.
        let d = [dict("cat", Some("dog"))];
        assert_eq!(process("concatenate the cat", &opts(&d, &[], "casual")).text, "Concatenate the dog");
    }

    #[test]
    fn dictionary_survives_capitalization_tone_and_ai_recasing() {
        let d = [dict("cube control", Some("kubectl")), dict("iphone", Some("iPhone")), dict("Postgres", None)];
        assert_eq!(process("cube control is down. iphone too", &opts(&d, &[], "formal")).text, "kubectl is down. iPhone too.");
        assert_eq!(process("Postgres is down", &opts(&d, &[], "very_casual")).text, "Postgres is down");
        // An AI cleanup that capitalized the output at a sentence start is put back.
        let p = process("Kubectl apply failed on IPHONE", &opts(&d, &[], "none"));
        assert_eq!((p.text.as_str(), p.fixes), ("kubectl apply failed on iPhone", 2));
        // An output that is a common word is only produced from its phrase, never forced elsewhere.
        let d = [dict("go lang", Some("Go"))];
        assert_eq!(process("go lang is fine, let's go", &opts(&d, &[], "none")).text, "Go is fine, let's go");
    }

    #[test]
    fn dictionary_precedence_and_overlaps() {
        // Longest phrase wins regardless of order, and one entry never rewrites another's output.
        for d in [[dict("j s", Some("JS")), dict("node j s", Some("Node.js"))], [dict("node j s", Some("Node.js")), dict("j s", Some("JS"))]] {
            assert_eq!(process("use node j s and j s", &opts(&d, &[], "none")).text, "Use Node.js and JS");
        }
        let d = [dict("nest dev", Some("feature/nest_dev")), dict("feature", Some("Feature"))];
        assert_eq!(process("merge nest dev, then the feature", &opts(&d, &[], "none")).text, "Merge feature/nest_dev, then the Feature");
        // A phrase beats another entry's output that reads the same.
        let d = [dict("see quill", Some("Sequel")), dict("sequel", Some("SQL"))];
        assert_eq!(process("write the sequel, see quill", &opts(&d, &[], "none")).text, "Write the SQL, Sequel");
        // Paths, URLs and file names aren't speech: left as typed.
        let d = [dict("github", Some("GitHub")), dict("postgres", Some("Postgres"))];
        assert_eq!(process("open github.com and src/postgres/ in github", &opts(&d, &[], "none")).text, "Open github.com and src/postgres/ in GitHub");
        assert_eq!(process("a postgres-compatible github-hosted db", &opts(&d, &[], "none")).text, "A Postgres-compatible GitHub-hosted db");
        assert_eq!(apply_dictionary("ask about cube control", &[dict("cube control", Some("kubectl"))]), "ask about kubectl");
        assert_eq!(apply_dictionary("unchanged", &[]), "unchanged");
    }

    fn code_opts<'a>(d: &'a [DictEntry]) -> Options<'a> {
        Options { technical: true, code: true, ..opts(d, &[], "none") }
    }

    #[test]
    fn spoken_symbols_in_code() {
        let o = code_opts(&[]);
        let t = |s: &str| process(s, &o).text;
        assert_eq!(t("open src slash app slash main dot rs"), "open src/app/main.rs");
        assert_eq!(t("set user underscore id and get underscore user"), "set user_id and get_user");
        assert_eq!(t("run dot slash build dot sh dash dash release"), "run ./build.sh --release");
        assert_eq!(t("cd tilde slash projects"), "cd ~/projects");
        assert_eq!(t("slash compact"), "/compact");
        assert_eq!(t("edit the dot env file and package dot JSON"), "edit the .env file and package.json");
        assert_eq!(t("self dot name"), "self.name");
        // Transcribers often spell short extensions letter by letter.
        assert_eq!(t("open main dot R S and app dot t s x"), "open main.rs and app.tsx");
        assert_eq!(t("point dot x y"), "point.x y", "not an extension: an ordinary member");
        assert_eq!(t("Windows path C colon backslash"), "Windows path C colon backslash", "nothing after it");
        // Literal words next to common words stay as said.
        assert_eq!(t("compute the dot product"), "compute the dot product");
        assert_eq!(t("to underscore the point, use the slash command"), "to underscore the point, use the slash command");
        assert_eq!(t("and slash or"), "and slash or");
        assert_eq!(t("join words with underscore characters"), "join words with underscore characters");
        assert_eq!(t("call to underscore string on click underscore handler"), "call to_string on click_handler");
    }

    #[test]
    fn spoken_symbols_outside_code() {
        let o = opts(&[], &[], "formal");
        let t = |s: &str| process(s, &o).text;
        // Unambiguous file and domain names join everywhere; identifiers don't.
        assert_eq!(t("check main dot rs on example dot com"), "Check main.rs on example.com.");
        assert_eq!(t("main dot rs is broken"), "main.rs is broken.");
        assert_eq!(t("user underscore id"), "User underscore id.");
        assert_eq!(t("the dot com era and the slash prices"), "The dot com era and the slash prices.");
        assert_eq!(t("polka dot dress"), "Polka dot dress.");
        // Off with voice commands.
        assert_eq!(process("main dot rs", &Options { voice_commands: false, ..o }).text, "Main dot rs.");
    }

    #[test]
    fn spoken_quotes_and_backticks() {
        let o = opts(&[], &[], "formal");
        assert_eq!(process("he said quote ship it end quote and left", &o).text, "He said \"ship it\" and left.");
        assert_eq!(process("call it quote, the plan, unquote.", &o).text, "Call it \"the plan\".");
        assert_eq!(process("run backtick npm install backtick first", &code_opts(&[])).text, "run `npm install` first");
        // An unpaired word is just a word.
        assert_eq!(process("send me a quote for the job", &o).text, "Send me a quote for the job.");
    }

    #[test]
    fn ai_prompts_wrap_code_in_backticks() {
        let o = Options { backticks: true, ..code_opts(&[]) };
        let t = |s: &str| process(s, &o).text;
        assert_eq!(t("fix the bug in src slash app slash main dot rs, then run it with dash dash watch."), "fix the bug in `src/app/main.rs`, then run it with `--watch`.");
        assert_eq!(t("rename user underscore id in (models dot py)"), "rename `user_id` in (`models.py`)");
        assert_eq!(t("load the dot env file, not .NET, section 2.c or the a.c."), "load the `.env` file, not .NET, section 2.c or the a.c.");
        // URLs, emails, product names, prose and existing backticks are left alone.
        let left = "see https://example.com/a_b, a@b.io, Next.js, and/or `main.rs` docs";
        assert_eq!(process(left, &Options { terms: true, ..o }).text, left);
    }

    #[test]
    fn dictionary_wins_over_symbols_and_backticks() {
        // A user's phrase containing a symbol word is theirs, not a spoken symbol.
        let d = [dict("dot net", Some(".NET")), dict("my slash command", Some("/deploy"))];
        assert_eq!(process("build asp dot net with my slash command", &code_opts(&d)).text, "build asp .NET with /deploy");
        let d = [dict("nest dev", Some("feature/nest_dev"))];
        let o = Options { backticks: true, ..code_opts(&d) };
        assert_eq!(process("checkout nest dev", &o).text, "checkout `feature/nest_dev`");
    }

    #[test]
    fn snippets() {
        let s = [Snippet { id: None, trigger: "my linkedin".into(), expansion: "https://linkedin.com/in/x".into(), created_at: None }];
        assert_eq!(process("My LinkedIn.", &opts(&[], &s, "formal")).text, "https://linkedin.com/in/x");
        assert_eq!(
            process("here is my linkedin thanks", &opts(&[], &s, "casual")).text,
            "Here is https://linkedin.com/in/x thanks"
        );
    }

    /// Cost on the dictation path: cargo test --release process_speed -- --ignored --nocapture
    #[test]
    #[ignore]
    fn process_speed() {
        let d: Vec<DictEntry> = (0..300).map(|i| dict(&format!("term {i} alias"), Some(&format!("Term{i}")))).collect();
        let text = "Open src slash app slash main dot rs and set user underscore id, then term 7 alias. ".repeat(12);
        let o = Options { terms: true, backticks: true, ..code_opts(&d) };
        crate::terms::warm();
        let t = std::time::Instant::now();
        for _ in 0..20 {
            std::hint::black_box(process(&text, &o));
        }
        println!("300 entries, {} words: {:?} per dictation", word_count(&text), t.elapsed() / 20);
    }

    /// Team library entries as the database hands them to dictation: the user's own always win.
    #[test]
    fn team_entries_never_beat_the_users_own() {
        let dir = std::env::temp_dir().join(format!("opstrellis-text-team-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let db = crate::db::Db::open(&dir.join("t.db")).unwrap();
        let own = |p: &str, r: Option<&str>| db.upsert_dictionary(dict(p, r)).unwrap();
        own("post gress", Some("Postgres"));
        own("kube", Some("Kubernetes"));
        db.upsert_snippet(Snippet { id: None, trigger: "my sig".into(), expansion: "Mine".into(), created_at: None }).unwrap();
        db.upsert_snippet(Snippet { id: None, trigger: "paste context".into(), expansion: "Context: {clipboard}".into(), created_at: None }).unwrap();
        let word = |id: &str, p: &str, r: &str| serde_json::json!({ "id": id, "kind": "dictionary", "data": { "phrase": p, "replacement": r } });
        let snip = |id: &str, t: &str, e: &str| serde_json::json!({ "id": id, "kind": "snippet", "data": { "trigger": t, "expansion": e } });
        let page = crate::team::Page {
            team: Some(crate::team::TeamRef { id: "t".into(), name: "T".into() }),
            version: 5,
            entries: vec![word("a", "post-gress", "PostgreSQL DB"), word("b", "Kubernetes", "K8s"), snip("c", "my  sig", "Team"), snip("d", "team intro", "Hi, paste context please")],
            more: false,
            reset: false,
        };
        crate::team::apply(&db, &crate::team::cursor(&db).unwrap(), &page).unwrap();
        let (d, s) = (db.list_dictionary().unwrap(), db.list_snippets().unwrap());
        let reads = std::sync::atomic::AtomicUsize::new(0);
        let read = || { reads.fetch_add(1, std::sync::atomic::Ordering::SeqCst); Some("secret".into()) };
        let o = Options { clipboard: Some(&read), ..opts(&d, &s, "none") };
        assert_eq!(process("ask post-gress", &o).text, "Ask Postgres", "the same phrase as the matcher compares it");
        assert_eq!(process("deploy to kube", &o).text, "Deploy to Kubernetes", "a team phrase can't rewrite the user's output");
        assert_eq!(process("thanks, my sig", &o).text, "Thanks, Mine");
        assert_eq!(process("so team intro", &o).text, "So Hi, paste context please", "a team expansion never triggers the user's snippet");
        assert_eq!(reads.load(std::sync::atomic::Ordering::SeqCst), 0);
        drop(db);
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// Snippets on the dictation path (team ones count, up to `db::TEAM_MAX_SNIPPETS`): cargo test --release snippet_speed -- --ignored --nocapture
    #[test]
    #[ignore]
    fn snippet_speed() {
        let text = "Can you send me the deck before the review tomorrow, and add the latest numbers. ".repeat(6);
        for n in [50, 100, 500, 1000] {
            let s: Vec<Snippet> = (0..n).map(|i| Snippet { id: None, trigger: format!("trigger phrase {i}"), expansion: format!("expansion {i}"), created_at: None }).collect();
            let o = opts(&[], &s, "none");
            let t = std::time::Instant::now();
            for _ in 0..10 {
                std::hint::black_box(process(&text, &o));
                std::hint::black_box(snippet_triggers_in(&text, &s));
            }
            println!("{n} snippets: {:?} per dictation (process + snippet_triggers_in)", t.elapsed() / 10);
        }
    }

    #[test]
    fn tones() {
        assert_eq!(apply_tone("Sounds good.", "casual"), "Sounds good");
        assert_eq!(apply_tone("Sounds good. See you.", "casual"), "Sounds good. See you.");
        assert_eq!(apply_tone("Sounds good. I will see NASA.", "very_casual"), "sounds good. I will see NASA");
        assert_eq!(apply_tone("We won.", "excited"), "We won!");
        assert_eq!(tidy("see example.com and v2.5. next", true), "See example.com and v2.5. Next");
        assert_eq!(tidy("main.rs is fine. example.com too. user_id", true), "main.rs is fine. example.com too. user_id");
        assert_eq!(apply_tone("Please review the document", "formal"), "Please review the document.");
        assert_eq!(apply_tone("Thanks", "formal"), "Thanks");
    }
}
