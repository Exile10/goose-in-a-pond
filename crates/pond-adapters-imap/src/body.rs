//! Turning a fetched message body into text worth indexing.
//!
//! What arrives from `BODY.PEEK[TEXT]` is a MIME entity: possibly multipart,
//! possibly quoted-printable or base64, usually with an HTML alternative, and
//! very often with a hundred lines of quoted reply and a signature block under
//! the four sentences that matter.
//!
//! Indexing that raw would fill the corpus with other people's footers. Every
//! message would carry the same "sent from my phone", the same unsubscribe
//! paragraph, the same legal boilerplate — and since those repeat across
//! hundreds of messages they would be the most FINDABLE text in the mailbox
//! while answering nothing.
//!
//! So this keeps the body's own words and drops the parts that belong to
//! everything else.

/// Extract readable text from a raw MIME body.
///
/// Prefers `text/plain` when the message offers both; falls back to stripping
/// tags from the HTML part. Decodes quoted-printable and base64 transfer
/// encodings, then trims quoted replies and signatures.
pub fn body_to_text(raw: &str) -> String {
    let (headers, content) = split_headers(raw);
    let boundary = mime_boundary(&headers);

    let text = match boundary {
        Some(b) => best_part(content, &b).unwrap_or_else(|| content.to_string()),
        None => decode_transfer(content, &headers),
    };

    let text = if looks_like_html(&text) {
        html_to_text(&text)
    } else {
        text
    };
    trim_reply_and_signature(&text)
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

fn header_value(headers: &str, name: &str) -> Option<String> {
    let lower = headers.to_ascii_lowercase();
    let at = lower.find(&format!("{}:", name.to_ascii_lowercase()))?;
    let line_end = headers[at..]
        .find('\n')
        .map(|i| at + i)
        .unwrap_or(headers.len());
    Some(
        headers[at..line_end]
            .splitn(2, ':')
            .nth(1)?
            .trim()
            .to_string(),
    )
}

fn mime_boundary(headers: &str) -> Option<String> {
    let ct = header_value(headers, "content-type")?;
    let at = ct.to_ascii_lowercase().find("boundary=")?;
    let rest = ct[at + "boundary=".len()..].trim();
    Some(rest.trim_matches(|c| c == '"' || c == ';').to_string())
}

/// The most useful part of a multipart body: plain text if there is any.
fn best_part(content: &str, boundary: &str) -> Option<String> {
    let sep = format!("--{boundary}");
    let mut plain = None;
    let mut html = None;
    for part in content.split(&sep) {
        let (h, body) = split_headers(part);
        let ct = header_value(&h, "content-type")
            .unwrap_or_default()
            .to_ascii_lowercase();
        let decoded = decode_transfer(body, &h);
        if ct.contains("text/plain") && plain.is_none() {
            plain = Some(decoded);
        } else if ct.contains("text/html") && html.is_none() {
            html = Some(html_to_text(&decoded));
        }
    }
    plain.or(html)
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

/// Quoted-printable as bodies use it: `=XX` escapes and `=` soft line breaks.
///
/// Distinct from the header form in `header.rs`, where `_` means a space and
/// there are no soft breaks. Sharing one decoder between them would corrupt
/// one or the other.
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

fn looks_like_html(text: &str) -> bool {
    let head = text.trim_start().to_ascii_lowercase();
    head.starts_with("<!doctype html") || head.starts_with("<html") || head.contains("<body")
}

/// Tags out, entities in, whitespace collapsed.
///
/// `script` and `style` contents are dropped rather than flattened: a page of
/// minified CSS is the single least useful thing that could be embedded, and it
/// is often the largest part of a marketing email.
fn html_to_text(html: &str) -> String {
    let mut out = String::with_capacity(html.len() / 2);
    let bytes = html.as_bytes();
    let mut i = 0;
    let lower = html.to_ascii_lowercase();

    while i < bytes.len() {
        if bytes[i] == b'<' {
            for tag in ["script", "style"] {
                let open = format!("<{tag}");
                if lower[i..].starts_with(&open) {
                    let close = format!("</{tag}>");
                    i = lower[i..]
                        .find(&close)
                        .map(|j| i + j + close.len())
                        .unwrap_or(bytes.len());
                    continue;
                }
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

/// Cut the quoted reply and the signature.
///
/// Both are text this message did not write. A thread of ten replies otherwise
/// stores the first message ten times, and the tenth copy is as findable as the
/// original — which is how a mailbox search starts returning the same paragraph
/// from a dozen different messages.
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

    #[test]
    fn a_body_that_is_only_a_signature_becomes_empty() {
        let raw = "Content-Type: text/plain\r\n\r\n-- \nJerry";
        assert!(body_to_text(raw).is_empty());
    }
}
