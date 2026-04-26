/// TTS text preprocessing — ported from pond-core/services/chat.rs
///
/// pond-desktop is NOT in the root Cargo workspace, so we can't import pond-core.
/// These functions are exact copies of the CLI's text processing pipeline.

// ── Whisper artifact stripping ──────────────────────────────────────────────

/// Remove Whisper non-speech tags (`[BLANK_AUDIO]`, `[MUSIC]`, `[NOISE]`, …)
/// and common hallucination phrases. Returns "" if nothing real remains.
///
/// Ported from pond-adapters-whisper `strip_whisper_artifacts()`.
pub fn strip_whisper_artifacts(text: &str) -> String {
    // Strip all [BRACKETED_TAGS]
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(open) = rest.find('[') {
        out.push_str(&rest[..open]);
        if let Some(close) = rest[open..].find(']') {
            rest = &rest[open + close + 1..];
        } else {
            rest = &rest[open..];
            break;
        }
    }
    out.push_str(rest);

    // Strip (PARENTHESIZED TAGS) — e.g. (inaudible), (music), (laughing)
    let mut cleaned = String::with_capacity(out.len());
    let mut prest = out.as_str();
    while let Some(open) = prest.find('(') {
        cleaned.push_str(&prest[..open]);
        if let Some(close) = prest[open..].find(')') {
            prest = &prest[open + close + 1..];
        } else {
            prest = &prest[open..];
            break;
        }
    }
    cleaned.push_str(prest);

    let cleaned = cleaned.trim();
    if cleaned.is_empty() {
        return String::new();
    }

    let lower = cleaned.to_lowercase();
    const EXACT_HALLUCINATIONS: &[&str] = &[
        ".", "..", "...", ",", "!", "?",
        "thank you", "thanks for watching", "thanks for listening",
        "thanks", "you", "bye", "bye bye", "okay",
        "the end", "subtitles by", "subtitle",
        "so", "um", "uh", "hmm", "huh", "ah", "oh",
        "i'm sorry", "i don't know",
        "please subscribe", "like and subscribe",
    ];
    if EXACT_HALLUCINATIONS.iter().any(|h| lower == *h) {
        return String::new();
    }

    // Very short transcripts (1-2 chars) are almost always noise.
    if cleaned.len() <= 2 {
        return String::new();
    }

    // Same word/syllable repeated = noise.
    let words: Vec<&str> = lower.split_whitespace().collect();
    if words.len() >= 2 && words.iter().all(|w| *w == words[0]) {
        return String::new();
    }

    cleaned.to_string()
}

/// Dismissal phrases that signal the user wants to end the conversation
/// and return to wake word mode. NOT a hard exit — just "go to sleep".
const DISMISSAL_PHRASES: &[&str] = &[
    "bye", "goodbye", "good bye", "dismissed", "go to sleep",
    "that's all", "thats all", "never mind", "nevermind",
    "stop", "stop listening",
];

/// Hard exit phrases that signal full voice pipeline shutdown.
const EXIT_PHRASES: &[&str] = &["exit", "quit"];

/// Check whether a transcript is a dismissal or exit command.
/// Returns `Some(farewell_text)` for dismissal/exit, `None` for normal speech.
pub fn check_dismissal(transcript: &str) -> Option<(&'static str, bool)> {
    let lower = transcript.trim().to_lowercase();
    let lower = lower.trim_end_matches(|c: char| c == '.' || c == '!');
    if DISMISSAL_PHRASES.iter().any(|p| lower == *p) {
        return Some(("Until next time. Just say my name when you need me.", false));
    }
    if EXIT_PHRASES.iter().any(|p| lower == *p) {
        return Some(("Goodbye! I'll be here whenever you need me.", true));
    }
    None
}

// ── Tool announcements ──────────────────────────────────────────────────────

/// Human-readable announcement spoken while an MCP tool is executing.
pub fn tool_announcement(tool: &str) -> String {
    let name = tool.split("__").last().unwrap_or(tool);
    match name {
        "get_current_weather" | "get_weather" => "Let me check the weather.".to_string(),
        "get_devices" | "list_devices"        => "Checking your devices.".to_string(),
        "set_schedule" | "create_schedule"    => "Setting that up.".to_string(),
        "save_memory"                         => "Got it, I'll remember that.".to_string(),
        other => format!("Let me {}.", other.replace('_', " ")),
    }
}

// ── Sentence splitting ──────────────────────────────────────────────────────

/// Split completed sentences out of a text buffer.
///
/// Sentence boundaries: `.`, `?`, `!` followed by whitespace or end-of-string,
/// and bare newlines. Forces a flush at 250 characters to handle code blocks
/// or long lists without sentence punctuation.
///
/// Returns `(sentences_to_speak, remaining_buffer)`.
pub fn split_sentences(text: &str) -> (Vec<String>, String) {
    const MAX_BUF: usize = 250;
    let mut sentences: Vec<String> = Vec::new();
    let mut remainder = text.to_string();

    loop {
        // Force-flush at max buffer: break at last space within the limit
        if remainder.len() > MAX_BUF {
            if let Some(split_at) = remainder[..MAX_BUF].rfind(' ') {
                sentences.push(remainder[..split_at].to_string());
                remainder = remainder[split_at + 1..].to_string();
                continue;
            }
        }

        let mut found = false;
        let chars: Vec<(usize, char)> = remainder.char_indices().collect();
        for (_idx, (i, ch)) in chars.iter().enumerate() {
            if matches!(ch, '.' | '?' | '!') {
                let next = i + ch.len_utf8();
                let after = &remainder[next..];
                if after.is_empty() || after.starts_with(' ') || after.starts_with('\n') {
                    sentences.push(remainder[..next].to_string());
                    remainder = after.trim_start_matches(|c: char| c == ' ' || c == '\n').to_string();
                    found = true;
                    break;
                }
            } else if *ch == '\n' {
                let chunk = remainder[..*i].trim().to_string();
                if !chunk.is_empty() {
                    sentences.push(chunk);
                }
                let next = i + 1;
                remainder = remainder[next..].to_string();
                found = true;
                break;
            }
        }

        if !found {
            break;
        }
    }

    (sentences, remainder)
}

// ── Markdown stripping ──────────────────────────────────────────────────────

/// Convert a markdown string to plain text suitable for TTS.
pub fn strip_markdown_for_speech(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut in_code_fence = false;

    for line in text.lines() {
        let trimmed = line.trim();

        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            in_code_fence = !in_code_fence;
            continue;
        }
        if in_code_fence {
            continue;
        }

        if is_hr(trimmed) {
            continue;
        }

        let content = strip_line_prefix(trimmed);
        let content = strip_inline_md(content);
        let content = content.trim().to_string();
        if !content.is_empty() {
            if !out.is_empty() {
                out.push(' ');
            }
            out.push_str(&content);
        }
    }

    normalize_for_speech(out.trim())
}

fn is_hr(s: &str) -> bool {
    if s.len() < 3 {
        return false;
    }
    let first = s.chars().next().unwrap_or(' ');
    if !matches!(first, '-' | '*' | '_') {
        return false;
    }
    s.chars().all(|c| c == first || c == ' ')
}

fn strip_line_prefix(line: &str) -> &str {
    if line.starts_with('#') {
        return line.trim_start_matches('#').trim_start();
    }
    if let Some(rest) = line.strip_prefix("> ").or_else(|| line.strip_prefix('>')) {
        return rest.trim_start();
    }
    if let Some(rest) = line.strip_prefix("- ")
        .or_else(|| line.strip_prefix("* "))
        .or_else(|| line.strip_prefix("+ "))
    {
        return rest;
    }
    let bytes = line.as_bytes();
    let mut i = 0;
    while i < bytes.len() && bytes[i].is_ascii_digit() {
        i += 1;
    }
    if i > 0 && bytes.get(i) == Some(&b'.') && bytes.get(i + 1) == Some(&b' ') {
        return &line[i + 2..];
    }
    line
}

fn strip_inline_md(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let chars: Vec<char> = s.chars().collect();
    let len = chars.len();
    let mut i = 0;

    while i < len {
        if chars[i] == '!' && chars.get(i + 1) == Some(&'[') {
            if let Some((end, _)) = find_link(&chars, i + 1) {
                i = end;
                continue;
            }
        }

        if chars[i] == '[' {
            if let Some((end, text)) = find_link(&chars, i) {
                out.push_str(&text);
                i = end;
                continue;
            }
        }

        if chars.get(i..i + 2) == Some(&['~', '~']) {
            if let Some(close) = find_marker_close(&chars, i + 2, &['~', '~']) {
                out.push_str(&chars[i + 2..close].iter().collect::<String>());
                i = close + 2;
                continue;
            }
        }

        if chars.get(i..i + 2) == Some(&['*', '*'])
            || chars.get(i..i + 2) == Some(&['_', '_'])
        {
            let marker = [chars[i], chars[i + 1]];
            if let Some(close) = find_marker_close(&chars, i + 2, &marker) {
                out.push_str(&chars[i + 2..close].iter().collect::<String>());
                i = close + 2;
                continue;
            }
        }

        if chars[i] == '*' || chars[i] == '_' {
            let marker = [chars[i]];
            if let Some(close) = find_marker_close(&chars, i + 1, &marker) {
                out.push_str(&chars[i + 1..close].iter().collect::<String>());
                i = close + 1;
                continue;
            }
        }

        if chars[i] == '`' {
            if let Some(close) = find_marker_close(&chars, i + 1, &['`']) {
                out.push_str(&chars[i + 1..close].iter().collect::<String>());
                i = close + 1;
                continue;
            }
        }

        out.push(chars[i]);
        i += 1;
    }

    out
}

fn find_link(chars: &[char], start: usize) -> Option<(usize, String)> {
    if chars.get(start) != Some(&'[') {
        return None;
    }
    let mut depth = 0usize;
    let mut j = start;
    while j < chars.len() {
        match chars[j] {
            '[' => depth += 1,
            ']' => {
                depth -= 1;
                if depth == 0 {
                    break;
                }
            }
            _ => {}
        }
        j += 1;
    }
    if j >= chars.len() {
        return None;
    }
    let text_end = j;
    if chars.get(j + 1) != Some(&'(') {
        return None;
    }
    let mut k = j + 2;
    let mut depth2 = 1usize;
    while k < chars.len() && depth2 > 0 {
        match chars[k] {
            '(' => depth2 += 1,
            ')' => depth2 -= 1,
            _ => {}
        }
        k += 1;
    }
    if depth2 != 0 {
        return None;
    }
    let link_text: String = chars[start + 1..text_end].iter().collect();
    Some((k, link_text))
}

fn find_marker_close(chars: &[char], start: usize, marker: &[char]) -> Option<usize> {
    let mlen = marker.len();
    let limit = chars.len().saturating_sub(mlen - 1);
    for i in start..limit {
        if &chars[i..i + mlen] == marker {
            return Some(i);
        }
    }
    None
}

// ── Symbol normalization ────────────────────────────────────────────────────

// ── Lookup tables (identical to pond-core/services/chat.rs) ─────────────────

/// Unit suffixes matched after a number. Sorted longest-first for greedy matching.
const UNIT_SUFFIXES: &[(&str, &str)] = &[
    // ── Compound / slash units ──
    ("km/h",  " kilometers per hour"),
    ("mi/h",  " miles per hour"),
    ("KB/s",  " kilobytes per second"),
    ("MB/s",  " megabytes per second"),
    ("m/s",   " meters per second"),
    ("ft/s",  " feet per second"),
    ("fl oz", " fluid ounces"),
    // ── Data (IEC binary) ──
    ("KiB", " kibibytes"), ("MiB", " mebibytes"), ("GiB", " gibibytes"), ("TiB", " tebibytes"),
    // ── Data speed ──
    ("kbps", " kilobits per second"), ("Mbps", " megabits per second"), ("Gbps", " gigabits per second"),
    // ── Energy (long) ──
    ("kWh", " kilowatt hours"), ("kcal", " kilocalories"), ("BTU", " B T U"),
    // ── Frequency ──
    ("THz", " terahertz"), ("GHz", " gigahertz"), ("MHz", " megahertz"), ("kHz", " kilohertz"),
    // ── Power ──
    ("GW", " gigawatts"), ("MW", " megawatts"), ("kW", " kilowatts"), ("mW", " milliwatts"),
    // ── Voltage ──
    ("kV", " kilovolts"), ("mV", " millivolts"),
    // ── Current ──
    ("mA", " milliamps"), ("μA", " microamps"),
    // ── Resistance ──
    ("MΩ", " megaohms"), ("kΩ", " kilohms"),
    // ── Pressure ──
    ("MPa", " megapascals"), ("kPa", " kilopascals"),
    ("mmHg", " millimeters of mercury"),
    ("atm", " atmospheres"), ("bar", " bar"), ("psi", " P S I"),
    // ── Energy ──
    ("MJ", " megajoules"), ("kJ", " kilojoules"),
    ("cal", " calories"), ("eV", " electron volts"), ("Wh", " watt hours"),
    // ── Sound ──
    ("dBA", " D B A"), ("dB", " decibels"),
    // ── Duration ──
    ("hrs", " hours"), ("sec", " seconds"), ("min", " minutes"),
    ("ms", " milliseconds"), ("ns", " nanoseconds"), ("μs", " microseconds"),
    ("hr", " hours"),
    // ── Data storage ──
    ("KB", " kilobytes"), ("MB", " megabytes"), ("GB", " gigabytes"),
    ("TB", " terabytes"), ("PB", " petabytes"), ("EB", " exabytes"),
    // ── Speed ──
    ("mph", " miles per hour"), ("bps", " bits per second"),
    // ── Area (with superscript) ──
    ("km²", " square kilometers"), ("cm²", " square centimeters"),
    ("m²", " square meters"), ("ft²", " square feet"), ("in²", " square inches"),
    ("cm³", " cubic centimeters"), ("m³", " cubic meters"),
    ("ha", " hectares"),
    // ── Length ──
    ("km", " kilometers"), ("cm", " centimeters"), ("mm", " millimeters"),
    ("nm", " nanometers"), ("μm", " micrometers"),
    ("mi", " miles"), ("ft", " feet"), ("yd", " yards"),
    // ── Weight ──
    ("kg", " kilograms"), ("mg", " milligrams"), ("μg", " micrograms"),
    ("lbs", " pounds"), ("lb", " pounds"), ("oz", " ounces"), ("st", " stone"),
    // ── Volume ──
    ("mL", " milliliters"), ("dL", " deciliters"), ("kL", " kiloliters"),
    ("gal", " gallons"), ("qt", " quarts"), ("pt", " pints"),
    // ── Single-char units (last — shortest match) ──
    ("Hz", " hertz"), ("Pa", " pascals"),
    ("W", " watts"), ("V", " volts"), ("A", " amps"),
    ("J", " joules"), ("Ω", " ohms"), ("L", " liters"),
    ("m", " meters"), ("g", " grams"),
];

/// Currency symbols: (char, singular, plural).
const CURRENCY_SYMBOLS: &[(char, &str, &str)] = &[
    ('$', "dollar",   "dollars"),
    ('£', "pound",    "pounds"),
    ('€', "euro",     "euros"),
    ('¥', "yen",      "yen"),
    ('₹', "rupee",    "rupees"),
    ('₽', "ruble",    "rubles"),
    ('₩', "won",      "won"),
    ('₪', "shekel",   "shekels"),
    ('₦', "naira",    "naira"),
    ('₱', "peso",     "pesos"),
    ('₺', "lira",     "lira"),
    ('₴', "hryvnia",  "hryvnias"),
    ('₵', "cedi",     "cedis"),
    ('₡', "colon",    "colones"),
    ('₫', "dong",     "dong"),
    ('₭', "kip",      "kip"),
    ('₮', "tugrik",   "tugriks"),
    ('₧', "peseta",   "pesetas"),
    ('₣', "franc",    "francs"),
];

/// Standalone single-character symbols.
const STANDALONE_SYMBOLS: &[(char, &str)] = &[
    ('±', "plus or minus "), ('×', " times "), ('÷', " divided by "),
    ('∞', "infinity"), ('≈', "approximately "), ('≤', "less than or equal to "),
    ('≥', "greater than or equal to "), ('≠', "not equal to "),
    ('√', "square root of "), ('π', "pi"),
    ('²', " squared"), ('³', " cubed"),
    ('½', "one half"), ('⅓', "one third"), ('⅔', "two thirds"),
    ('¼', "one quarter"), ('¾', "three quarters"),
    ('⅕', "one fifth"), ('⅖', "two fifths"), ('⅗', "three fifths"), ('⅘', "four fifths"),
    ('⅙', "one sixth"), ('⅚', "five sixths"),
    ('⅛', "one eighth"), ('⅜', "three eighths"), ('⅝', "five eighths"), ('⅞', "seven eighths"),
    ('©', "copyright"), ('®', "registered"), ('™', "trademark"),
    ('§', "section"), ('¶', "paragraph"),
    ('†', ""), ('‡', ""),
    ('•', ", "),
    ('—', ", "),
];

/// Common abbreviations with periods.
const ABBREVIATIONS: &[(&str, &str)] = &[
    ("e.g.",  "for example"), ("i.e.",  "that is"), ("etc.",  "etcetera"),
    ("vs.",   "versus"), ("approx.", "approximately"),
    ("dept.", "department"), ("govt.", "government"),
    ("inc.",  "incorporated"), ("corp.", "corporation"), ("ltd.",  "limited"),
    ("prof.", "professor"), ("dr.", "doctor"), ("mr.", "mister"),
    ("mrs.",  "missus"), ("ms.", "miss"), ("jr.", "junior"), ("sr.", "senior"),
    ("st.",   "saint"), ("ave.", "avenue"), ("blvd.", "boulevard"),
    ("ft.",   "fort"), ("mt.", "mount"),
    ("no.",   "number"), ("vol.", "volume"), ("ch.", "chapter"),
    ("pg.",   "page"), ("fig.", "figure"),
    ("max.",  "maximum"), ("min.", "minimum"), ("temp.", "temperature"),
    ("est.",  "established"),
    ("jan.", "January"), ("feb.", "February"), ("mar.", "March"),
    ("apr.", "April"), ("jun.", "June"), ("jul.", "July"),
    ("aug.", "August"), ("sep.", "September"), ("oct.", "October"),
    ("nov.", "November"), ("dec.", "December"),
];

/// Convert symbols and abbreviations to their spoken equivalents.
pub fn normalize_for_speech(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let len = chars.len();
    let mut out = String::with_capacity(len + len / 4);
    let mut i = 0;

    while i < len {
        let ch = chars[i];

        // ── Time: 3:45pm / 15:30 / 9am ──
        if ch.is_ascii_digit() && (i == 0 || !chars[i - 1].is_ascii_digit()) {
            if let Some((spoken, advance)) = try_read_time(&chars, i) {
                out.push_str(&spoken);
                i += advance;
                continue;
            }
        }

        // ── Unit suffix after number: 5kg → "5 kilograms" ──
        if i > 0 && chars[i - 1].is_ascii_digit() && !ch.is_ascii_digit() {
            if let Some((spoken, advance)) = try_read_unit_suffix(&chars, i) {
                out.push_str(&spoken);
                i += advance;
                continue;
            }
        }

        // ── Abbreviations: e.g. → "for example", Dr. → "doctor" ──
        if ch.is_alphabetic() {
            if let Some((expansion, advance)) = try_read_abbreviation(&chars, i) {
                out.push_str(&expansion);
                i += advance;
                continue;
            }
        }

        // ── Period / dot — context-dependent ──
        if ch == '.' {
            let prev_digit = i > 0 && chars[i - 1].is_ascii_digit();
            let next_digit = chars.get(i + 1).map_or(false, |c| c.is_ascii_digit());
            if prev_digit && next_digit {
                let mut j = i + 1;
                while j < len && chars[j].is_ascii_digit() { j += 1; }
                let has_unit = try_read_unit_suffix(&chars, j).is_some();
                if has_unit {
                    out.push(ch);
                    i += 1;
                    continue;
                }
                out.push_str(" point ");
                i += 1;
                continue;
            }
            let prev_alpha = i > 0 && chars[i - 1].is_alphabetic();
            let next_alpha = chars.get(i + 1).map_or(false, |c| c.is_alphabetic());
            if prev_alpha && next_alpha {
                out.push_str(" dot ");
                i += 1;
                continue;
            }
            out.push(ch);
            i += 1;
            continue;
        }

        // ── Degree symbol ──
        if ch == '°' {
            match chars.get(i + 1) {
                Some('C') | Some('c') => { out.push_str(" degrees Celsius");    i += 2; continue; }
                Some('F') | Some('f') => { out.push_str(" degrees Fahrenheit"); i += 2; continue; }
                Some('K') | Some('k') => { out.push_str(" kelvin");             i += 2; continue; }
                _                     => { out.push_str(" degrees");            i += 1; continue; }
            }
        }

        // ── Percent ──
        if ch == '%' { out.push_str(" percent"); i += 1; continue; }

        // ── Currency symbols (table-driven) ──
        if let Some(&(_, singular, plural)) = CURRENCY_SYMBOLS.iter().find(|&&(c, _, _)| c == ch) {
            let (num_str, advance) = read_number(&chars, i + 1);
            if advance > 0 {
                let is_one = num_str == "1" || num_str == "1.0" || num_str == "1.00";
                let unit = if is_one { singular } else { plural };
                out.push_str(&num_str);
                out.push(' ');
                out.push_str(unit);
                i += 1 + advance;
                continue;
            }
        }

        // ── Ampersand ──
        if ch == '&' {
            let prev_space = i == 0 || chars[i - 1].is_whitespace();
            let next_space = chars.get(i + 1).map_or(true, |c| c.is_whitespace());
            if prev_space || next_space { out.push_str("and"); i += 1; continue; }
        }

        // ── At-sign ──
        if ch == '@' {
            let prev_space = i == 0 || chars[i - 1].is_whitespace();
            let next_space = chars.get(i + 1).map_or(true, |c| c.is_whitespace());
            if prev_space || next_space { out.push_str("at"); i += 1; continue; }
        }

        // ── Number sign: #5 → "number 5" ──
        if ch == '#' && chars.get(i + 1).map_or(false, |c| c.is_ascii_digit()) {
            out.push_str("number "); i += 1; continue;
        }

        // ── En dash: 3–5 → "3 to 5", otherwise a pause ──
        if ch == '–' {
            let prev_digit = i > 0 && chars[i - 1].is_ascii_digit();
            let next_digit = chars.get(i + 1).map_or(false, |c| c.is_ascii_digit());
            if prev_digit && next_digit { out.push_str(" to "); } else { out.push_str(", "); }
            i += 1; continue;
        }

        // ── Standalone symbol table ──
        if let Some(&(_, spoken)) = STANDALONE_SYMBOLS.iter().find(|&&(c, _)| c == ch) {
            out.push_str(spoken); i += 1; continue;
        }

        out.push(ch);
        i += 1;
    }

    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Try to match a unit suffix at position `i` (immediately after a number ended).
fn try_read_unit_suffix(chars: &[char], i: usize) -> Option<(String, usize)> {
    let len = chars.len();
    let (unit_start, space_consumed) = if i < len && chars[i] == ' ' {
        (i + 1, 1usize)
    } else {
        (i, 0usize)
    };
    if unit_start >= len { return None; }

    for &(suffix, spoken) in UNIT_SUFFIXES {
        let suffix_chars: Vec<char> = suffix.chars().collect();
        let slen = suffix_chars.len();
        if unit_start + slen > len { continue; }
        let matches = suffix_chars.iter().enumerate().all(|(j, &sc)| chars[unit_start + j] == sc);
        if !matches { continue; }
        let after = unit_start + slen;
        if after < len && chars[after].is_alphabetic() { continue; }
        return Some((spoken.to_string(), space_consumed + slen));
    }
    None
}

/// Try to match an abbreviation starting at position `i` (word boundary).
fn try_read_abbreviation(chars: &[char], i: usize) -> Option<(String, usize)> {
    let len = chars.len();
    let at_boundary = i == 0
        || chars[i - 1].is_whitespace()
        || matches!(chars[i - 1], '(' | ',' | '"' | '\'' | '[');
    if !at_boundary { return None; }

    let window_end = (i + 10).min(len);
    let window: String = chars[i..window_end]
        .iter()
        .map(|c| c.to_ascii_lowercase())
        .collect();

    for &(abbrev, expansion) in ABBREVIATIONS {
        if window.starts_with(abbrev) {
            let consumed = abbrev.chars().count();
            return Some((expansion.to_string(), consumed));
        }
    }
    None
}

fn read_number(chars: &[char], start: usize) -> (String, usize) {
    let mut j = start;
    while j < chars.len() && chars[j].is_ascii_digit() {
        j += 1;
    }
    if chars.get(j) == Some(&'.') && chars.get(j + 1).map_or(false, |c| c.is_ascii_digit()) {
        j += 1;
        while j < chars.len() && chars[j].is_ascii_digit() {
            j += 1;
        }
    }
    if j == start {
        return (String::new(), 0);
    }
    let s: String = chars[start..j].iter().collect();
    (s, j - start)
}

fn try_read_time(chars: &[char], start: usize) -> Option<(String, usize)> {
    let len = chars.len();
    let mut j = start;

    let h_start = j;
    while j < len && chars[j].is_ascii_digit() && j - h_start < 2 {
        j += 1;
    }
    if j == h_start { return None; }
    let hour: u32 = chars[h_start..j].iter().collect::<String>().parse().ok()?;
    if hour > 23 { return None; }
    let hour_str: String = chars[h_start..j].iter().collect();

    let mut minute_str: Option<String> = None;
    if chars.get(j) == Some(&':') {
        let d1 = chars.get(j + 1)?;
        let d2 = chars.get(j + 2)?;
        if d1.is_ascii_digit() && d2.is_ascii_digit() {
            let min: u32 = format!("{}{}", d1, d2).parse().ok()?;
            if min > 59 { return None; }
            minute_str = Some(format!("{}{}", d1, d2));
            j += 3;
        } else {
            return None;
        }
    }

    let ws_j = j;
    while j < len && chars[j] == ' ' {
        j += 1;
    }

    let ampm = if j + 1 < len {
        let a = chars[j].to_ascii_lowercase();
        let b = chars[j + 1].to_ascii_lowercase();
        if (a == 'a' || a == 'p') && b == 'm' {
            if chars.get(j + 2).map_or(true, |c| !c.is_alphabetic()) {
                j += 2;
                Some(if a == 'a' { "AM" } else { "PM" })
            } else {
                None
            }
        } else {
            None
        }
    } else {
        None
    };

    if minute_str.is_none() && ampm.is_none() {
        return None;
    }

    if ampm.is_none() {
        j = ws_j;
    }

    let mut spoken = hour_str;
    if let Some(ref m) = minute_str {
        if m != "00" || ampm.is_none() {
            spoken.push(' ');
            spoken.push_str(m);
        }
    }
    if let Some(ap) = ampm {
        spoken.push(' ');
        spoken.push_str(ap);
    }

    Some((spoken, j - start))
}

// ── Thinking block filter ───────────────────────────────────────────────────

/// Strip `<think>…</think>` reasoning blocks from a streaming text chunk.
///
/// `in_block` is the carry-over state from the previous chunk.
/// Returns `(visible_text, updated_in_block)`.
pub fn filter_thinking(chunk: &str, mut in_block: bool) -> (String, bool) {
    let mut visible = String::with_capacity(chunk.len());
    let mut rest = chunk;

    loop {
        if in_block {
            if let Some(end) = rest.find("</think>") {
                rest = &rest[end + "</think>".len()..];
                in_block = false;
            } else {
                break;
            }
        } else {
            if let Some(start) = rest.find("<think>") {
                visible.push_str(&rest[..start]);
                rest = &rest[start + "<think>".len()..];
                in_block = true;
            } else {
                visible.push_str(rest);
                break;
            }
        }
    }

    (visible, in_block)
}
