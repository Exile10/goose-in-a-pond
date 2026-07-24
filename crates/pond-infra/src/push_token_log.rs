//! Safe redaction of push tokens for logging, shared by the push relays.
//!
//! A push token is client-supplied (registered over `POST
//! /api/v1/devices/{id}/push-token`), so the relays must never assume it is
//! ASCII. Slicing it by byte index — `&token[..8]` — panics when byte 8 lands
//! inside a multi-byte character, and the relays run inline inside
//! `BroadcastNotificationSender::send`, which catches relay *errors* but not
//! panics. Truncating by character keeps that "best-effort" promise honest.

/// How much of a push token is safe to log (prefix only).
const TOKEN_LOG_PREFIX_CHARS: usize = 8;

/// The leading [`TOKEN_LOG_PREFIX_CHARS`] characters of a push token, for
/// correlating log lines without recording the credential itself. Never
/// panics, whatever bytes the token holds.
pub(crate) fn token_log_prefix(token: &str) -> &str {
    match token.char_indices().nth(TOKEN_LOG_PREFIX_CHARS) {
        Some((byte_idx, _)) => &token[..byte_idx],
        None => token,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncates_a_long_ascii_token() {
        assert_eq!(token_log_prefix("abcdefghijklmnop"), "abcdefgh");
    }

    #[test]
    fn returns_short_tokens_whole() {
        assert_eq!(token_log_prefix("abc"), "abc");
        assert_eq!(token_log_prefix(""), "");
    }

    /// Exactly [`TOKEN_LOG_PREFIX_CHARS`] characters: no truncation, and no
    /// off-by-one panic at the boundary.
    #[test]
    fn handles_the_exact_boundary() {
        assert_eq!(token_log_prefix("abcdefgh"), "abcdefgh");
    }

    /// The regression this module exists for: byte 8 falls inside a multi-byte
    /// character, so `&token[..8]` would panic.
    #[test]
    fn does_not_panic_on_multi_byte_tokens() {
        // "éééé…" — every char is 2 bytes, so byte index 8 is a boundary but
        // the first 8 *chars* are 16 bytes.
        assert_eq!(token_log_prefix("éééééééééé"), "éééééééé");
        // Byte 8 lands mid-character here (3-byte chars).
        assert_eq!(token_log_prefix("日本語のトークンです"), "日本語のトークン");
        // Mixed widths, and an emoji spanning 4 bytes.
        assert_eq!(token_log_prefix("ab\u{1F600}cdefghij"), "ab\u{1F600}cdefg");
    }
}
