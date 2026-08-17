//! Decoding the two header fields this connector keeps.
//!
//! A subject is the whole payload of a mail context item — `embedding_text` is
//! `title\nbody` and the title is the subject — so a subject left as
//! `=?UTF-8?B?...?=` is not a cosmetic problem. It is a corpus row that reads
//! as line noise to a person and embeds as line noise to a retriever.

use base64::Engine;

/// Decode RFC 2047 encoded-words in a header value.
///
/// Handles both `B` (base64) and `Q` (quoted-printable) encodings, leaves
/// anything it cannot decode exactly as it found it, and joins adjacent
/// encoded-words without the separating whitespace the RFC says to drop.
pub fn decode_rfc2047(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut rest = raw;
    // Whether the previous token was an encoded-word: RFC 2047 says whitespace
    // BETWEEN two encoded-words is not part of the text, which is how a subject
    // split across several words rejoins without gaps.
    let mut prev_was_encoded = false;

    while let Some(start) = rest.find("=?") {
        let (before, tail) = rest.split_at(start);
        if !(prev_was_encoded && before.trim().is_empty()) {
            out.push_str(before);
        }
        // charset ? encoding ? text ?=
        let Some(end) = tail[2..].find("?=").map(|i| i + 2) else {
            out.push_str(tail);
            return out;
        };
        let word = &tail[2..end];
        let mut parts = word.splitn(3, '?');
        let (Some(_charset), Some(encoding), Some(text)) =
            (parts.next(), parts.next(), parts.next())
        else {
            out.push_str(&tail[..end + 2]);
            rest = &tail[end + 2..];
            prev_was_encoded = false;
            continue;
        };
        let decoded = match encoding.to_ascii_uppercase().as_str() {
            "B" => base64::engine::general_purpose::STANDARD
                .decode(text)
                .ok()
                .map(|b| String::from_utf8_lossy(&b).into_owned()),
            "Q" => Some(decode_q(text)),
            _ => None,
        };
        match decoded {
            // An encoded-word this pond cannot read is left verbatim rather
            // than dropped: a subject somebody can squint at beats a blank one.
            Some(text) => {
                out.push_str(&text);
                prev_was_encoded = true;
            }
            None => {
                out.push_str(&tail[..end + 2]);
                prev_was_encoded = false;
            }
        }
        rest = &tail[end + 2..];
    }
    out.push_str(rest);
    out
}

/// Quoted-printable as RFC 2047 uses it, where `_` means a space.
fn decode_q(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'_' => {
                out.push(b' ');
                i += 1;
            }
            b'=' if i + 2 < bytes.len() => {
                let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or("");
                match u8::from_str_radix(hex, 16) {
                    Ok(b) => {
                        out.push(b);
                        i += 3;
                    }
                    Err(_) => {
                        out.push(bytes[i]);
                        i += 1;
                    }
                }
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_text_is_untouched() {
        assert_eq!(decode_rfc2047("Dentist appointment"), "Dentist appointment");
    }

    #[test]
    fn base64_words_are_decoded() {
        assert_eq!(
            decode_rfc2047("=?UTF-8?B?SGFiYXJpIHlha28=?="),
            "Habari yako"
        );
    }

    #[test]
    fn quoted_printable_words_are_decoded_with_underscore_as_space() {
        assert_eq!(decode_rfc2047("=?utf-8?Q?Rent_due?="), "Rent due");
        assert_eq!(decode_rfc2047("=?utf-8?Q?caf=C3=A9?="), "café");
    }

    /// RFC 2047: whitespace between two encoded-words is not part of the text.
    /// Without this a long subject comes back with gaps in the middle of words.
    #[test]
    fn adjacent_encoded_words_rejoin_without_the_separating_space() {
        assert_eq!(
            decode_rfc2047("=?utf-8?Q?Habari?= =?utf-8?Q?_yako?="),
            "Habari yako"
        );
    }

    #[test]
    fn text_around_an_encoded_word_is_kept() {
        assert_eq!(
            decode_rfc2047("Re: =?utf-8?Q?caf=C3=A9?= tomorrow"),
            "Re: café tomorrow"
        );
    }

    /// A subject somebody can squint at beats a blank one, so anything
    /// undecodable is left exactly as it arrived.
    #[test]
    fn an_unreadable_encoded_word_is_left_verbatim() {
        assert_eq!(
            decode_rfc2047("=?utf-8?X?something?="),
            "=?utf-8?X?something?="
        );
        assert_eq!(decode_rfc2047("=?truncated"), "=?truncated");
    }
}
