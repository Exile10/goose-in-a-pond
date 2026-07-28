//! Per-turn limits for image attachments (phase F1).
//!
//! # Why a hard cap exists at all
//!
//! An image turn is the most expensive thing this system can be asked to do on
//! an 8 GB Jetson Orin Nano:
//!
//! 1. Vision turns bypass the engine's retained KV prompt-session cache
//!    (`inference_engine.rs` drops the retained session before a multimodal
//!    prefill), so every image turn pays a FULL prefill — there is no
//!    amortisation across turns.
//! 2. Each image is decoded to RGB and run through the mmproj vision encoder
//!    before tokenisation. A 12 MP phone photo is ~36 MB of RGB in a device
//!    that has ~1 GB of headroom left after the model and KV cache.
//! 3. Every image expands into a few hundred prompt tokens, and prefill on the
//!    Orin runs at roughly 0.9K tok/s.
//!
//! So an unbounded request is not "slow", it is an OOM. The caps below are
//! deliberately generous for a home-assistant use case and deliberately far
//! below what a phone camera produces unaided — clients are expected to
//! downscale before encoding (longest edge 1024 px, see
//! `pond-desktop/src/lib/imageAttach.ts`), and these caps are the server-side
//! backstop for clients that do not.

use super::message::ImageAttachment;

/// Maximum number of images accepted in a single chat turn.
///
/// Four is the point where a 2B-class vision encoder still finishes in a couple
/// of seconds on the Orin and the added prompt tokens stay inside the 4096-token
/// context the Jetson tuning block pins. It is also the frame cap used by the
/// video sampler (phase F5) so a sampled clip and a manual attachment cost the
/// same worst case.
pub const MAX_IMAGES_PER_TURN: usize = 4;

/// Maximum decoded (post-base64) size of a single image, in bytes.
///
/// 4 MiB of compressed JPEG/PNG is far more than a 1024 px-longest-edge image
/// needs (typically 100-400 KiB) but leaves room for a lossless PNG screenshot.
pub const MAX_IMAGE_BYTES: usize = 4 * 1024 * 1024;

/// Maximum decoded size of ALL images in one turn, in bytes.
///
/// Bounds the peak transient allocation for a single request independently of
/// the per-image cap, so `MAX_IMAGES_PER_TURN` images at `MAX_IMAGE_BYTES` each
/// cannot be combined into a 16 MiB spike.
pub const MAX_TOTAL_IMAGE_BYTES: usize = 8 * 1024 * 1024;

/// Request-body ceiling for the chat routes, in bytes.
///
/// Axum's `DefaultBodyLimit` is 2 MiB, which is SMALLER than a legal attachment
/// set: a request carrying `MAX_TOTAL_IMAGE_BYTES` of images is ~4/3 that size
/// once base64-encoded. Without raising it, a perfectly legal 3 MB attachment is
/// rejected by the framework with "length limit exceeded" and the caller never
/// reaches the checks in this module, which are the ones that can say something
/// useful. This is the framework backstop; [`validate_turn_images`] is the
/// policy, and it should be what a user actually hits.
///
/// Sized so that EVERY rejection in this module is reachable, not just the
/// per-image one: `MAX_IMAGES_PER_TURN` images that are each individually legal
/// must get through the framework so `TotalTooLarge` can explain the aggregate
/// budget. That is a larger transient buffer than the budget itself
/// (`4 x 4 MiB` base64-inflated, ~22 MiB), which is the deliberate cost of a
/// good error message; it is bounded, short-lived, and the concurrency of these
/// routes is already capped by the SSE semaphore.
pub const MAX_CHAT_BODY_BYTES: usize =
    (MAX_IMAGES_PER_TURN * MAX_IMAGE_BYTES) * 4 / 3 + 1024 * 1024;

// The backstop must sit above EVERY policy limit, or the policy never runs and
// the caller gets Axum's "length limit exceeded" instead of an actionable
// message. Compile-time rather than a test: it is a relationship between
// constants, so a violation should not build. The first version of
// MAX_CHAT_BODY_BYTES was sized off MAX_TOTAL_IMAGE_BYTES and made
// `TotalTooLarge` unreachable over HTTP; this is the guard against that.
const _: () = assert!(MAX_CHAT_BODY_BYTES > MAX_TOTAL_IMAGE_BYTES * 4 / 3);
const _: () = assert!(MAX_CHAT_BODY_BYTES > (MAX_IMAGES_PER_TURN * MAX_IMAGE_BYTES) * 4 / 3);
// Axum's own default is 2 MiB, which is below a single legal image.
const _: () = assert!(MAX_CHAT_BODY_BYTES > 2 * 1024 * 1024);

/// MIME types the mtmd vision path can decode.
///
/// Kept explicit rather than accepting any `image/*`: an unsupported container
/// fails deep inside the engine with a much worse error than a 415 here.
pub const SUPPORTED_IMAGE_MIME_TYPES: &[&str] = &[
    "image/jpeg",
    "image/png",
    "image/webp",
    "image/gif",
    "image/bmp",
];

/// Why a turn's image attachments were rejected.
///
/// Deliberately carries the offending numbers so the HTTP layer can render an
/// actionable message ("3.2 MB, limit is 4.0 MB") instead of "bad request".
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImageLimitError {
    TooManyImages {
        count: usize,
        max: usize,
    },
    ImageTooLarge {
        index: usize,
        bytes: usize,
        max: usize,
    },
    TotalTooLarge {
        bytes: usize,
        max: usize,
    },
    UnsupportedMimeType {
        index: usize,
        mime_type: String,
    },
    EmptyImage {
        index: usize,
    },
}

impl std::fmt::Display for ImageLimitError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TooManyImages { count, max } => write!(
                f,
                "too many images in one message: {count} attached, at most {max} are supported per turn"
            ),
            Self::ImageTooLarge { index, bytes, max } => write!(
                f,
                "image {} is {:.1} MB, which exceeds the {:.1} MB per-image limit — resize it before sending",
                index + 1,
                *bytes as f64 / (1024.0 * 1024.0),
                *max as f64 / (1024.0 * 1024.0),
            ),
            Self::TotalTooLarge { bytes, max } => write!(
                f,
                "attachments total {:.1} MB, which exceeds the {:.1} MB limit for one message",
                *bytes as f64 / (1024.0 * 1024.0),
                *max as f64 / (1024.0 * 1024.0),
            ),
            Self::UnsupportedMimeType { index, mime_type } => write!(
                f,
                "image {} has unsupported type \"{}\" — supported types are {}",
                index + 1,
                mime_type,
                SUPPORTED_IMAGE_MIME_TYPES.join(", ")
            ),
            Self::EmptyImage { index } => {
                write!(f, "image {} carries no data", index + 1)
            }
        }
    }
}

impl std::error::Error for ImageLimitError {}

impl ImageLimitError {
    /// `true` when the right HTTP status is 413 Payload Too Large rather than
    /// 400/415. Lets the API layer pick a status without matching variants.
    pub fn is_too_large(&self) -> bool {
        matches!(
            self,
            Self::ImageTooLarge { .. } | Self::TotalTooLarge { .. } | Self::TooManyImages { .. }
        )
    }
}

/// Decoded byte length of a base64 payload, without decoding it.
///
/// Standard base64 encodes 3 bytes per 4 characters; each trailing `=` removes
/// one output byte. Whitespace (which some clients insert every 76 chars) is
/// not counted. This is an estimate only in the sense that it trusts the input
/// to be well-formed base64 — it is exact for valid input, and the point is to
/// reject an oversized payload BEFORE allocating a decode buffer for it.
#[must_use]
pub fn decoded_len(base64: &str) -> usize {
    let mut chars = 0usize;
    let mut padding = 0usize;
    for b in base64.bytes() {
        match b {
            b' ' | b'\n' | b'\r' | b'\t' => {}
            b'=' => {
                chars += 1;
                padding += 1;
            }
            _ => chars += 1,
        }
    }
    // Unpadded base64 (some clients omit `=`): 2 trailing chars -> 1 byte, 3 -> 2.
    let remainder = match chars % 4 {
        2 => 1,
        3 => 2,
        _ => 0,
    };
    ((chars / 4) * 3 + remainder).saturating_sub(padding.min(2))
}

/// Validate a turn's image attachments against the per-request limits.
///
/// Pure: no allocation beyond the error path, no decoding. Call this before the
/// request reaches the agent so an oversized payload is a 4xx and not an OOM.
pub fn validate_turn_images(images: &[ImageAttachment]) -> Result<(), ImageLimitError> {
    if images.len() > MAX_IMAGES_PER_TURN {
        return Err(ImageLimitError::TooManyImages {
            count: images.len(),
            max: MAX_IMAGES_PER_TURN,
        });
    }

    let mut total = 0usize;
    for (index, img) in images.iter().enumerate() {
        let mime = img.mime_type.trim().to_ascii_lowercase();
        // Tolerate a charset/parameter suffix, e.g. "image/jpeg; charset=binary".
        let base = mime.split(';').next().unwrap_or("").trim();
        if !SUPPORTED_IMAGE_MIME_TYPES.contains(&base) {
            return Err(ImageLimitError::UnsupportedMimeType {
                index,
                mime_type: img.mime_type.clone(),
            });
        }

        let bytes = decoded_len(&img.data);
        if bytes == 0 {
            return Err(ImageLimitError::EmptyImage { index });
        }
        if bytes > MAX_IMAGE_BYTES {
            return Err(ImageLimitError::ImageTooLarge {
                index,
                bytes,
                max: MAX_IMAGE_BYTES,
            });
        }
        total = total.saturating_add(bytes);
    }

    if total > MAX_TOTAL_IMAGE_BYTES {
        return Err(ImageLimitError::TotalTooLarge {
            bytes: total,
            max: MAX_TOTAL_IMAGE_BYTES,
        });
    }

    Ok(())
}

/// File extension for a supported image MIME type, without the dot.
///
/// Used when persisting an attachment to disk (phase F2) so the stored file is
/// openable by a human debugging a session.
#[must_use]
pub fn extension_for_mime(mime_type: &str) -> &'static str {
    match mime_type
        .trim()
        .to_ascii_lowercase()
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
    {
        "image/png" => "png",
        "image/webp" => "webp",
        "image/gif" => "gif",
        "image/bmp" => "bmp",
        // JPEG and anything that slipped past validation land here; a `.jpg`
        // that is really something else is still readable by every viewer that
        // sniffs magic bytes.
        _ => "jpg",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn img(bytes: usize, mime: &str) -> ImageAttachment {
        // 4 base64 chars per 3 bytes, rounded up, then padded to a multiple of 4.
        let groups = bytes.div_ceil(3);
        ImageAttachment {
            data: "A".repeat(groups * 4),
            mime_type: mime.to_string(),
        }
    }

    #[test]
    fn decoded_len_matches_real_base64_lengths() {
        // "hello" -> "aGVsbG8=" (5 bytes, one pad)
        assert_eq!(decoded_len("aGVsbG8="), 5);
        // "hell"  -> "aGVsbA==" (4 bytes, two pads)
        assert_eq!(decoded_len("aGVsbA=="), 4);
        // "hel"   -> "aGVs"     (3 bytes, no pad)
        assert_eq!(decoded_len("aGVs"), 3);
        assert_eq!(decoded_len(""), 0);
    }

    #[test]
    fn decoded_len_ignores_wrapping_whitespace() {
        assert_eq!(decoded_len("aGVs\naGVs\n"), 6);
    }

    #[test]
    fn decoded_len_handles_unpadded_input() {
        assert_eq!(decoded_len("aGVsbG8"), 5);
    }

    #[test]
    fn no_images_is_valid() {
        assert!(validate_turn_images(&[]).is_ok());
    }

    #[test]
    fn max_images_is_accepted_and_one_more_is_not() {
        let ok: Vec<_> = (0..MAX_IMAGES_PER_TURN)
            .map(|_| img(1024, "image/jpeg"))
            .collect();
        assert!(validate_turn_images(&ok).is_ok());

        let too_many: Vec<_> = (0..MAX_IMAGES_PER_TURN + 1)
            .map(|_| img(1024, "image/jpeg"))
            .collect();
        assert_eq!(
            validate_turn_images(&too_many),
            Err(ImageLimitError::TooManyImages {
                count: MAX_IMAGES_PER_TURN + 1,
                max: MAX_IMAGES_PER_TURN,
            })
        );
    }

    #[test]
    fn oversized_single_image_is_rejected_with_its_index() {
        let images = vec![
            img(1024, "image/png"),
            img(MAX_IMAGE_BYTES + 4096, "image/png"),
        ];
        match validate_turn_images(&images) {
            Err(ImageLimitError::ImageTooLarge { index, bytes, max }) => {
                assert_eq!(index, 1);
                assert!(bytes > MAX_IMAGE_BYTES);
                assert_eq!(max, MAX_IMAGE_BYTES);
            }
            other => panic!("expected ImageTooLarge, got {other:?}"),
        }
    }

    #[test]
    fn total_budget_rejects_several_individually_legal_images() {
        // Three images just under the per-image cap pass individually but blow
        // the aggregate budget.
        let each = MAX_IMAGE_BYTES - 1024;
        let images: Vec<_> = (0..3).map(|_| img(each, "image/jpeg")).collect();
        for one in &images {
            assert!(validate_turn_images(std::slice::from_ref(one)).is_ok());
        }
        match validate_turn_images(&images) {
            Err(ImageLimitError::TotalTooLarge { max, .. }) => {
                assert_eq!(max, MAX_TOTAL_IMAGE_BYTES)
            }
            other => panic!("expected TotalTooLarge, got {other:?}"),
        }
    }

    #[test]
    fn unsupported_and_empty_payloads_are_rejected() {
        assert_eq!(
            validate_turn_images(&[img(64, "application/pdf")]),
            Err(ImageLimitError::UnsupportedMimeType {
                index: 0,
                mime_type: "application/pdf".to_string(),
            })
        );
        assert_eq!(
            validate_turn_images(&[ImageAttachment {
                data: String::new(),
                mime_type: "image/png".to_string(),
            }]),
            Err(ImageLimitError::EmptyImage { index: 0 })
        );
    }

    #[test]
    fn mime_type_matching_is_case_and_parameter_tolerant() {
        assert!(validate_turn_images(&[img(64, "IMAGE/JPEG")]).is_ok());
        assert!(validate_turn_images(&[img(64, "image/png; charset=binary")]).is_ok());
    }

    #[test]
    fn size_errors_map_to_payload_too_large() {
        assert!(ImageLimitError::TooManyImages { count: 9, max: 4 }.is_too_large());
        assert!(!ImageLimitError::UnsupportedMimeType {
            index: 0,
            mime_type: "x".into()
        }
        .is_too_large());
    }

    #[test]
    fn extensions_cover_every_supported_mime() {
        assert_eq!(extension_for_mime("image/png"), "png");
        assert_eq!(extension_for_mime("image/webp"), "webp");
        assert_eq!(extension_for_mime("image/gif"), "gif");
        assert_eq!(extension_for_mime("image/bmp"), "bmp");
        assert_eq!(extension_for_mime("image/jpeg"), "jpg");
        assert_eq!(extension_for_mime("IMAGE/PNG; q=1"), "png");
    }
}
