//! Turning a fetched message body into text worth indexing. What arrives from
//! `BODY.PEEK[TEXT]` is a MIME entity with quoted replies, signatures and boilerplate under the
//! sentences that matter; indexed raw, the text repeated across hundreds of messages becomes
//! the most findable thing in the mailbox. This keeps the body's own words and drops the rest.

/// Extract readable text from a raw MIME body. Prefers `text/plain` over a tag-stripped HTML
/// part, decodes quoted-printable and base64 transfer encodings, then trims quoted replies
/// and signatures.
pub fn body_to_text(raw: &str) -> String {
    let (headers, content) = split_headers(raw);

    // Two ways to learn the boundary, and the sniffed one is what production hits:
    // `BODY.PEEK[TEXT]` omits the message headers, so the top-level `Content-Type` that names
    // the boundary is absent. Decoding the multipart as one blob leaves a QP soft break inside
    // `<sty=\r\nle ...>` and the stylesheet survives as text.
    let text = mime_boundary(&headers)
        .and_then(|b| best_part(content, &b))
        // Sniffed from `raw`, not `content`: the first part's headers have
        // already been eaten by `split_headers`, and the parts must be split
        // from the delimiter that precedes them.
        .or_else(|| sniff_boundary(raw).and_then(|b| best_part(raw, &b)))
        .unwrap_or_else(|| decode_transfer(content, &headers));

    let text = if looks_like_html(&text) {
        html_to_text(&text)
    } else {
        text
    };
    let text = trim_reply_and_signature(&text);
    strip_machine_noise(&text)
}

fn split_headers(raw: &str) -> (String, &str) {
    match raw
        .find("\r\n\r\n")
        .map(|i| (i, 4))
        .or(raw.find("\n\n").map(|i| (i, 2)))
    {
        Some((i, skip)) => (raw[..i].to_string(), &raw[i + skip..]),
        None => (String::new(), raw),
    }
}

/// One header's value, with folded continuation lines joined back on. RFC 5322 lets a long
/// header wrap onto lines starting with a space or tab, and servers do so constantly for
/// `Content-Type`; reading only to the first newline loses `boundary=`, so the parts never
/// split and the whole raw multipart, stylesheet included, is stored as the body.
fn header_value(headers: &str, name: &str) -> Option<String> {
    let lower = headers.to_ascii_lowercase();
    let at = lower.find(&format!("{}:", name.to_ascii_lowercase()))?;

    let mut value = String::new();
    for (n, line) in headers[at..].split('\n').enumerate() {
        let line = line.trim_end_matches('\r');
        if n == 0 {
            value.push_str(line.split_once(':')?.1);
        } else if line.starts_with(' ') || line.starts_with('\t') {
            // A fold is a line break inserted for width; the value continues.
            value.push(' ');
            value.push_str(line.trim());
        } else {
            break;
        }
    }
    Some(value.trim().to_string())
}

/// The boundary as written in the body itself, for a body that arrived without its message
/// headers: a multipart opens with its own delimiter, so the first non-empty line is
/// `--<boundary>`. Guarded against the `-- ` signature separator, the one other thing in
/// mail that begins a line with two dashes.
fn sniff_boundary(raw: &str) -> Option<String> {
    let first = raw.lines().find(|l| !l.trim().is_empty())?.trim_end();
    let token = first.strip_prefix("--")?;
    let token = token.strip_suffix("--").unwrap_or(token);
    let ok = token.len() >= 3
        && !token.contains(char::is_whitespace)
        && token.chars().any(|c| c.is_ascii_alphanumeric());
    ok.then(|| token.to_string())
}

fn mime_boundary(headers: &str) -> Option<String> {
    let ct = header_value(headers, "content-type")?;
    let at = ct.to_ascii_lowercase().find("boundary=")?;
    let rest = ct[at + "boundary=".len()..].trim();
    // A parameter ends at the next `;`, not at the end of the header --
    // `boundary="xyz"; charset=UTF-8` is ordinary. `trim_matches` only strips
    // the OUTER characters, so it yielded `xyz"; charset=UTF-8`: a boundary
    // that matches nothing, leaving the parts unsplit.
    let rest = if let Some(stripped) = rest.strip_prefix('"') {
        stripped.split('"').next().unwrap_or("")
    } else {
        rest.split(|c: char| c == ';' || c.is_whitespace())
            .next()
            .unwrap_or("")
    };
    let rest = rest.trim();
    (!rest.is_empty()).then(|| rest.to_string())
}

/// The most useful part of a multipart body: plain text if there is any.
fn best_part(content: &str, boundary: &str) -> Option<String> {
    let mut plain = None;
    let mut html = None;
    collect_parts(content, boundary, 0, &mut plain, &mut html);
    plain.or(html)
}

/// Walk the part tree, keeping the first text/plain and the first text/html. The tree is the
/// point: large senders wrap `multipart/alternative` in `multipart/related`, and a single-level
/// scan sees only the `multipart/*` wrapper and falls back to storing the raw body. Depth is
/// bounded because a mail body is not worth a stack overflow.
fn collect_parts(
    content: &str,
    boundary: &str,
    depth: usize,
    plain: &mut Option<String>,
    html: &mut Option<String>,
) {
    if depth > 8 || (plain.is_some() && html.is_some()) {
        return;
    }
    let sep = format!("--{boundary}");
    for part in content.split(&sep) {
        let (h, body) = split_headers(part);
        let ct = header_value(&h, "content-type")
            .unwrap_or_default()
            .to_ascii_lowercase();

        if ct.contains("multipart/") {
            // A wrapper carries its own boundary; the equality guard stops a
            // malformed part that repeats its parent's from recursing on
            // itself.
            match mime_boundary(&h) {
                Some(inner) if inner != boundary => {
                    collect_parts(body, &inner, depth + 1, plain, html);
                }
                _ => {}
            }
            continue;
        }

        let decoded = decode_transfer(body, &h);
        if ct.contains("text/plain") && plain.is_none() {
            *plain = Some(decoded);
        } else if ct.contains("text/html") && html.is_none() {
            *html = Some(html_to_text(&decoded));
        }
    }
}

fn decode_transfer(body: &str, headers: &str) -> String {
    match header_value(headers, "content-transfer-encoding")
        .unwrap_or_default()
        .to_ascii_lowercase()
        .trim()
    {
        "base64" => {
            use base64::Engine;
            let joined: String = body.split_whitespace().collect();
            base64::engine::general_purpose::STANDARD
                .decode(joined)
                .map(|b| String::from_utf8_lossy(&b).into_owned())
                .unwrap_or_else(|_| body.to_string())
        }
        "quoted-printable" => decode_qp(body),
        _ => body.to_string(),
    }
}

/// Quoted-printable as bodies use it: `=XX` escapes and `=` soft line breaks. Distinct from
/// the header form in `header.rs`, where `_` means a space and there are no soft breaks;
/// sharing one decoder would corrupt one or the other.
fn decode_qp(text: &str) -> String {
    let mut out: Vec<u8> = Vec::with_capacity(text.len());
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'=' && i + 1 < bytes.len() {
            // Soft line break: `=` at end of line joins the next one.
            if bytes[i + 1] == b'\n' {
                i += 2;
                continue;
            }
            if bytes[i + 1] == b'\r' && i + 2 < bytes.len() && bytes[i + 2] == b'\n' {
                i += 3;
                continue;
            }
            if i + 2 < bytes.len() {
                if let Ok(b) =
                    u8::from_str_radix(std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or(""), 16)
                {
                    out.push(b);
                    i += 3;
                    continue;
                }
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Whether this text carries markup worth stripping. Deliberately not anchored to the start:
/// a multipart whose boundary failed to parse begins with readable text and turns into markup
/// thousands of bytes later. Checked over a bounded prefix so a huge body pays once.
fn looks_like_html(text: &str) -> bool {
    let window = &text[..text.len().min(64 * 1024)];
    let lower = window.to_ascii_lowercase();
    lower.contains("<!doctype html")
        || lower.contains("<html")
        || lower.contains("<body")
        || lower.contains("<style")
        || lower.contains("<div")
        || lower.contains("<table")
}

/// Tags out, entities in, whitespace collapsed. `script` and `style` contents are dropped
/// rather than flattened: minified CSS is often the largest part of a marketing email and the
/// least useful thing that could be embedded.
fn html_to_text(html: &str) -> String {
    let mut out = String::with_capacity(html.len() / 2);
    let bytes = html.as_bytes();
    let mut i = 0;
    let lower = html.to_ascii_lowercase();

    while i < bytes.len() {
        if bytes[i] == b'<' {
            // `continue` inside the `for` continued the FOR loop, not this
            // one: after skipping a style block it fell through to the generic
            // tag handler with `i` no longer on a `<`, which then deleted
            // everything up to the next `>` -- real text, silently.
            let mut skipped = false;
            for tag in ["script", "style"] {
                let open = format!("<{tag}");
                if lower[i..].starts_with(&open) {
                    let close = format!("</{tag}>");
                    i = lower[i..]
                        .find(&close)
                        .map(|j| i + j + close.len())
                        .unwrap_or(bytes.len());
                    skipped = true;
                    break;
                }
            }
            if skipped {
                continue;
            }
            let end = lower[i..]
                .find('>')
                .map(|j| i + j + 1)
                .unwrap_or(bytes.len());
            // Block-level tags become breaks so paragraphs survive as paragraphs.
            let tag = &lower[i..end];
            if tag.starts_with("<p")
                || tag.starts_with("<br")
                || tag.starts_with("<div")
                || tag.starts_with("</p")
                || tag.starts_with("</div")
                || tag.starts_with("<tr")
            {
                out.push('\n');
            }
            i = end;
            continue;
        }
        out.push(bytes[i] as char);
        i += 1;
    }

    let decoded = out
        .replace("&nbsp;", " ")
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'");
    collapse_blank_lines(&decoded)
}

fn collapse_blank_lines(text: &str) -> String {
    let mut out: Vec<&str> = Vec::new();
    let mut blank = false;
    for line in text.lines() {
        let t = line.trim();
        if t.is_empty() {
            if !blank && !out.is_empty() {
                out.push("");
            }
            blank = true;
        } else {
            out.push(t);
            blank = false;
        }
    }
    out.join("\n").trim().to_string()
}

/// Cut the quoted reply and the signature. Both are text this message did not write: a
/// thread of ten replies would otherwise store the first message ten times, each copy as
/// findable as the original.
fn trim_reply_and_signature(text: &str) -> String {
    let mut cut = text.len();

    // `-- ` on its own line is the RFC-blessed signature marker.
    let mut offset = 0usize;
    for line in text.split_inclusive('\n') {
        let t = line.trim_end();
        let is_sig = t == "-- " || t == "--";
        let is_quote_header = (t.starts_with("On ") && t.ends_with("wrote:"))
            || t.starts_with("-----Original Message-----")
            || t.starts_with("________________________________");
        if is_sig || is_quote_header {
            cut = cut.min(offset);
            break;
        }
        offset += line.len();
    }

    let kept = &text[..cut];
    // Drop `>` quoted lines wherever they appear — some clients interleave.
    let unquoted: Vec<&str> = kept
        .lines()
        .filter(|l| !l.trim_start().starts_with('>'))
        .collect();
    collapse_blank_lines(&unquoted.join("\n"))
}

/// Reduce a URL to its host, and drop the other machine-readable debris. Measured: a job
/// alert carried ~90 characters of content and 600+ of tracking URL, so the vector described
/// a `trackingId` and the redactor took it for an API key. The host is kept because it is the
/// one part of a URL a person would search for.
fn strip_machine_noise(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;

    while let Some(at) = rest.find("http") {
        let (before, tail) = rest.split_at(at);
        out.push_str(before);
        if !(tail.starts_with("http://") || tail.starts_with("https://")) {
            out.push_str(&tail[..4]);
            rest = &tail[4..];
            continue;
        }
        let end = tail
            .find(|c: char| c.is_whitespace() || c == '<' || c == '>' || c == '"')
            .unwrap_or(tail.len());
        let url = &tail[..end];
        let host = url
            .trim_start_matches("https://")
            .trim_start_matches("http://")
            .split('/')
            .next()
            .unwrap_or("")
            .trim_start_matches("www.");
        if !host.is_empty() {
            out.push_str(host);
        }
        rest = &tail[end..];
    }
    out.push_str(rest);

    // Invisible characters: marketing mail is full of zero-width spaces used to
    // defeat spam filters, and they survive every other step here.
    let cleaned: String = out
        .chars()
        .filter(|c| !matches!(c, '\u{200b}'..='\u{200f}' | '\u{feff}' | '\u{00ad}'))
        .collect();

    // Machine output, by two tests: a low letter ratio catches separator rows and punctuation
    // soup but not base64, which is mostly letters, so a run of 30-plus characters without a
    // space is also dropped. Short lines are exempt so "Kenya" and "$4,200" survive.
    let kept: Vec<&str> = cleaned
        .lines()
        .filter(|line| {
            let t = line.trim();
            if t.is_empty() {
                return true;
            }
            // A part header that reached the text means the split missed it.
            // These lines are never something a person wrote.
            const MIME_HEADERS: [&str; 4] = [
                "content-type:",
                "content-transfer-encoding:",
                "content-disposition:",
                "content-id:",
            ];
            let lower = t.to_ascii_lowercase();
            if MIME_HEADERS.iter().any(|h| lower.starts_with(h)) {
                return false;
            }

            let chars = t.chars().count();
            let longest_run = t
                .split_whitespace()
                .map(|w| w.chars().count())
                .max()
                .unwrap_or(0);
            if longest_run > 30 {
                return false;
            }
            let letters = t.chars().filter(|c| c.is_alphabetic()).count();
            letters * 2 >= chars || chars < 12
        })
        .collect();

    collapse_blank_lines(&kept.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_body_passes_through() {
        let raw = "Content-Type: text/plain\r\n\r\nThe rent is due on the first.";
        assert_eq!(body_to_text(raw), "The rent is due on the first.");
    }

    #[test]
    fn quoted_printable_is_decoded_including_soft_breaks() {
        let raw = "Content-Transfer-Encoding: quoted-printable\r\n\r\nCaf=C3=A9 at =\r\nnoon";
        assert_eq!(body_to_text(raw), "Café at noon");
    }

    #[test]
    fn base64_bodies_are_decoded() {
        let raw = "Content-Transfer-Encoding: base64\r\n\r\nSGVsbG8gd29ybGQ=";
        assert_eq!(body_to_text(raw), "Hello world");
    }

    /// A marketing email is mostly CSS. Embedding it would make minified style
    /// rules the most findable text in the mailbox.
    #[test]
    fn style_and_script_blocks_are_dropped_not_flattened() {
        let raw = "Content-Type: text/html\r\n\r\n<html><style>.a{color:red}</style>\
                   <body><p>Your invoice is ready.</p><script>track()</script></body></html>";
        let out = body_to_text(raw);
        assert!(out.contains("Your invoice is ready."), "{out}");
        assert!(!out.contains("color:red"), "{out}");
        assert!(!out.contains("track()"), "{out}");
    }

    /// A wrapped Content-Type is the common case, not an edge case, and
    /// missing the fold meant the boundary was never found: the whole raw
    /// multipart became the body, stylesheet included.
    #[test]
    fn a_folded_content_type_still_yields_its_boundary() {
        let raw = "Content-Type: multipart/alternative;\r\n\
                   \tboundary=\"000000000000abc\"\r\n\r\n\
                   --000000000000abc\r\n\
                   Content-Type: text/plain\r\n\r\n\
                   The invoice is attached.\r\n\
                   --000000000000abc\r\n\
                   Content-Type: text/html\r\n\r\n\
                   <html><body>The invoice is attached.</body></html>\r\n\
                   --000000000000abc--\r\n";
        assert_eq!(body_to_text(raw), "The invoice is attached.");
    }

    /// `boundary="xyz"; charset=UTF-8` -- the parameter ends at the `;`.
    #[test]
    fn a_boundary_followed_by_another_parameter_is_not_swallowed() {
        let raw = "Content-Type: multipart/alternative; boundary=\"xyz\"; charset=UTF-8\r\n\r\n\
                   --xyz\r\n\
                   Content-Type: text/plain\r\n\r\n\
                   Rent is due Friday.\r\n\
                   --xyz--\r\n";
        assert_eq!(body_to_text(raw), "Rent is due Friday.");
    }

    /// Measured: 850 lines, 246 of them a bare `}`. When part-splitting fails
    /// the readable text comes first and the stylesheet follows, so a
    /// markup test anchored to the START of the body reports "not HTML".
    #[test]
    fn a_stylesheet_after_readable_text_is_still_dropped() {
        let raw = "Content-Type: text/plain\r\n\r\n\
                   Your job alert for full-stack developer\n\
                   <style>\n\
                   @media (max-width: 767px) {\n\
                   .notification-toasts.animate {\n\
                   transform: translate(0px, 0px) rotate(0) skewX(0) scaleY(1);\n\
                   }\n\
                   }\n\
                   </style>";
        let out = body_to_text(raw);
        assert!(out.contains("Your job alert"), "{out}");
        assert!(!out.contains("@media"), "stylesheet survived: {out}");
        assert!(!out.contains("transform"), "stylesheet survived: {out}");
    }

    /// The structure a large sender actually uses: related wrapping
    /// alternative wrapping the text. A single-level scan sees only the
    /// wrapper and stores the whole raw body instead.
    #[test]
    fn a_nested_multipart_is_walked_to_its_text_part() {
        let raw = "Content-Type: multipart/related; boundary=\"OUT\"\r\n\r\n\
                   --OUT\r\n\
                   Content-Type: multipart/alternative; boundary=\"IN\"\r\n\r\n\
                   --IN\r\n\
                   Content-Type: text/plain\r\n\r\n\
                   Your job alert for full-stack developer\r\n\
                   --IN\r\n\
                   Content-Type: text/html;charset=UTF-8\r\n\r\n\
                   <style>@media (max-width: 480px) { .x { display: none; } }</style>\r\n\
                   --IN--\r\n\
                   --OUT--\r\n";
        let out = body_to_text(raw);
        assert_eq!(out, "Your job alert for full-stack developer");
    }

    /// A part header inside the text means the split missed a boundary. Even
    /// then the header line itself is not content.
    #[test]
    fn a_stray_part_header_is_not_kept_as_text() {
        let raw = "Content-Type: text/plain\r\n\r\n\
                   Rent is due Friday.\n\
                   Content-Type: text/html;charset=UTF-8\n\
                   Content-Transfer-Encoding: quoted-printable\n\
                   Content-ID: html-body";
        assert_eq!(body_to_text(raw), "Rent is due Friday.");
    }

    /// What `BODY.PEEK[TEXT]` actually returns: the body without the message headers, so the
    /// top-level `Content-Type` that names the boundary is absent. The parts have different
    /// encodings, so one global decode leaves a QP soft break inside `<sty=\r\nle ...>` and
    /// the stylesheet survives as text. The boundary must be sniffed from the first line.
    #[test]
    fn a_body_without_message_headers_still_finds_its_boundary() {
        let raw = "--00000000000041cd\r\n\
                   Content-Type: text/plain; charset=UTF-8\r\n\
                   Content-Transfer-Encoding: 7bit\r\n\r\n\
                   Your job alert for full-stack developer\r\n\
                   --00000000000041cd\r\n\
                   Content-Type: text/html; charset=UTF-8\r\n\
                   Content-Transfer-Encoding: quoted-printable\r\n\r\n\
                   <sty=\r\nle type=3D\"text/css\">@media (max-width: 480px) { .x { display: n=\r\none; } }</sty=\r\nle>\r\n\
                   <div>Your job alert for full-stack developer</div>\r\n\
                   --00000000000041cd--\r\n";
        let out = body_to_text(raw);
        assert!(out.contains("Your job alert"), "{out}");
        assert!(!out.contains("@media"), "stylesheet survived: {out}");
        assert!(
            !out.contains("Content-Transfer-Encoding"),
            "part headers survived: {out}"
        );
    }

    #[test]
    fn multipart_prefers_the_plain_part() {
        let raw = "Content-Type: multipart/alternative; boundary=\"xyz\"\r\n\r\n\
                   --xyz\r\nContent-Type: text/html\r\n\r\n<p>html version</p>\r\n\
                   --xyz\r\nContent-Type: text/plain\r\n\r\nplain version\r\n--xyz--";
        assert_eq!(body_to_text(raw), "plain version");
    }

    /// A ten-deep thread would otherwise store the first message ten times, and
    /// every copy is as findable as the original.
    #[test]
    fn quoted_replies_are_cut() {
        let raw = "Content-Type: text/plain\r\n\r\nYes, Tuesday works.\n\n\
                   On Mon, 4 Aug 2026, Liz wrote:\n> Are you free Tuesday?\n> Liz";
        let out = body_to_text(raw);
        assert_eq!(out, "Yes, Tuesday works.");
    }

    #[test]
    fn signatures_are_cut_at_the_marker() {
        let raw = "Content-Type: text/plain\r\n\r\nSee you then.\n-- \nJerry Ochieng\nJarida";
        assert_eq!(body_to_text(raw), "See you then.");
    }

    #[test]
    fn interleaved_quote_lines_go_even_without_a_header() {
        let raw = "Content-Type: text/plain\r\n\r\n> old text\nmy reply\n> more old";
        assert_eq!(body_to_text(raw), "my reply");
    }

    /// Measured on real mail: 90 characters of job, 600 of trackingId. The
    /// vector would have described the tracking token.
    #[test]
    fn a_tracking_url_becomes_its_host() {
        let raw = "Content-Type: text/plain\r\n\r\nFull-Stack Developer (Remote)\nHire Feed\n\
                   View job: https://www.linkedin.com/comm/jobs/view/4441936354/?trackingId=\
                   5wNheFCzn51zjtsxOZA9vg%3D%3D&refId=Jsivf9Mo0pKZakE5ae8iGA%3D%3D&midToken=AQGp";
        let out = body_to_text(raw);
        assert!(out.contains("Full-Stack Developer (Remote)"), "{out}");
        assert!(
            out.contains("linkedin.com"),
            "the host is worth keeping: {out}"
        );
        assert!(!out.contains("trackingId"), "{out}");
        assert!(out.len() < 120, "still carrying the query string: {out}");
    }

    #[test]
    fn zero_width_padding_is_removed() {
        let raw = "Content-Type: text/plain\r\n\r\nYour\u{200b}invoice\u{feff} is ready";
        assert_eq!(body_to_text(raw), "Yourinvoice is ready");
    }

    /// A row of base64 or separators reads as nothing and searches as nothing.
    #[test]
    fn lines_that_are_mostly_not_letters_are_dropped() {
        let raw = "Content-Type: text/plain\r\n\r\nThe invoice is attached.\n\
                   ================================\n\
                   AQGp-NBSLWW61Q==2QXlOj0621Pck1==AQGp-NBSLWW61Q";
        let out = body_to_text(raw);
        assert_eq!(out, "The invoice is attached.");
    }

    /// Short lines survive the letter test — "Kenya", "$4,200", "Q3" are all
    /// content, and a rule that judged them by letter ratio would cut them.
    #[test]
    fn short_lines_survive_even_when_they_are_not_words() {
        let raw = "Content-Type: text/plain\r\n\r\nSalary\n$4,200\nKenya";
        let out = body_to_text(raw);
        assert!(out.contains("$4,200"), "{out}");
        assert!(out.contains("Kenya"), "{out}");
    }

    #[test]
    fn a_body_that_is_only_a_signature_becomes_empty() {
        let raw = "Content-Type: text/plain\r\n\r\n-- \nJerry";
        assert!(body_to_text(raw).is_empty());
    }
}
