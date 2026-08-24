//! Reading a GGUF file's own account of itself.
//!
//! A model dropped into the models folder by hand arrives with nothing but a
//! filename. The catalogue has slots for what it is — architecture, context
//! window, quantisation, parameter count — and every one of them was `None`,
//! so the page showed "(detected on disk)" beside a size and stopped.
//!
//! All of that is in the file. GGUF opens with a key/value header describing
//! the model, and it is the first thing in the file, so answering these
//! questions costs one short read rather than loading several gigabytes.
//!
//! # Shape of the header
//!
//! ```text
//! magic "GGUF"   u32
//! version        u32
//! tensor_count   u64
//! kv_count       u64
//! kv_count × { key: string, type: u32, value: <type> }
//! ```
//!
//! Strings are a `u64` length followed by that many bytes. Arrays are an
//! element type, a `u64` count, then the elements. Everything is
//! little-endian.
//!
//! This parser is deliberately total: every read is bounds-checked and any
//! malformed field ends the walk and returns what was understood so far. A
//! file in the models folder is arbitrary bytes from the internet, and the
//! worst outcome of a truncated or hostile header should be a card with less
//! on it — never a panic in a filesystem scan.

/// What a GGUF file says about itself. Every field is optional: headers vary
/// by architecture and by the tool that wrote them.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct GgufInfo {
    /// `general.architecture` — "llama", "gemma3", "qwen3", …
    pub architecture: Option<String>,
    /// `general.name` — the name the publisher gave it.
    pub name: Option<String>,
    /// Quantisation, resolved from `general.file_type`. "Q4_K_M", "F16", …
    pub quantization: Option<String>,
    /// `{arch}.context_length` — the window the weights were trained for.
    pub context_length: Option<u32>,
    /// `{arch}.embedding_length` — embedding width.
    pub embedding_length: Option<u32>,
    /// `{arch}.block_count` — transformer layers.
    pub block_count: Option<u32>,
    /// `general.parameter_count`, when the writer recorded it.
    pub parameter_count: Option<u64>,
    /// Tensors in the file. Always present in a well-formed header.
    pub tensor_count: Option<u64>,

    // ── Attention geometry, for KV-cache arithmetic ─────────────────────────
    //
    // These sit in the first ~2 KB of every file measured (offsets 924-1,829
    // on gemma-4-E2B), i.e. long before `tokenizer.ggml.tokens`, so a short
    // head read reaches all of them.
    /// `{arch}.attention.head_count_kv` — KV heads, the multiplier on cache size.
    pub head_count_kv: Option<u32>,
    /// `{arch}.attention.key_length` — K width per head, full-attention layers.
    pub key_length: Option<u32>,
    /// `{arch}.attention.value_length` — V width per head, full-attention layers.
    pub value_length: Option<u32>,
    /// `{arch}.attention.key_length_swa` — K width on sliding-window layers,
    /// where present. Gemma 4 halves it (256 against 512).
    pub key_length_swa: Option<u32>,
    /// `{arch}.attention.value_length_swa`, where present.
    pub value_length_swa: Option<u32>,
    /// `{arch}.attention.shared_kv_layers` — layers that share another layer's
    /// KV and therefore allocate none of their own. Gemma 4 E4B shares 18 of
    /// 42; E2B shares 20 of 35.
    pub shared_kv_layers: Option<u32>,
}

impl GgufInfo {
    /// Did the header yield anything worth showing?
    pub fn is_empty(&self) -> bool {
        *self == GgufInfo::default()
    }

    /// Bytes of KV cache this model needs per token of context, computed from
    /// its own header.
    ///
    /// # Why this is worth having
    ///
    /// `jetson_context_size` divides the memory budget by a per-token KV cost
    /// to decide a context window, and that constant carries a comment saying
    /// it "moves on a measurement from the Orin and nothing less" -- because
    /// getting it wrong OOM-killed the board once, and because a figure
    /// measured on a Mac understated the real cost by roughly three times.
    ///
    /// It does not have to be measured. It is arithmetic over four keys that
    /// sit in the first two kilobytes of the file, and it reproduces both
    /// device measurements exactly (see the tests): E2B 18 KiB/token, E4B 56.
    /// That turns "measure every new model on the hardware or risk the board"
    /// into something answerable before the weights are read.
    ///
    /// # The shape of the sum
    ///
    /// Only layers that own KV allocate any: `block_count - shared_kv_layers`.
    /// Of those, sliding-window layers use the narrower `*_swa` widths where
    /// the architecture declares them. Each layer stores K and V for every KV
    /// head at two bytes an element (f16, the default cache type).
    ///
    /// # The part that is inferred rather than read
    ///
    /// The split between full-attention and sliding-window layers is **not**
    /// in these headers. Both Gemma 4 models measured 1 global to 5 SWA
    /// (E4B 4+20 of 24, E2B 3+12 of 15), and that ratio is assumed here via
    /// `swa_per_global`. An architecture with a different pattern needs its own
    /// value, so this returns `None` rather than guessing when the widths that
    /// would make the answer wrong are absent.
    ///
    /// Returns `None` when the header lacks what the sum needs -- callers keep
    /// their conservative fallback rather than receiving a confident wrong
    /// number.
    pub fn kv_bytes_per_token(&self, swa_per_global: u32) -> Option<u64> {
        const BYTES_PER_ELEMENT: u64 = 2; // f16 cache

        let blocks = self.block_count?;
        let kv_heads = u64::from(self.head_count_kv?);
        let k = u64::from(self.key_length?);
        let v = u64::from(self.value_length?);

        let owning = blocks.saturating_sub(self.shared_kv_layers.unwrap_or(0));
        if owning == 0 || kv_heads == 0 {
            return None;
        }

        // No SWA widths declared: every owning layer pays the full width.
        let (Some(k_swa), Some(v_swa)) = (
            self.key_length_swa.or(self.key_length),
            self.value_length_swa.or(self.value_length),
        ) else {
            return Some(u64::from(owning) * (k + v) * kv_heads * BYTES_PER_ELEMENT);
        };

        let group = swa_per_global.saturating_add(1);
        let (global, swa) = if group <= 1 || self.key_length_swa.is_none() {
            (owning, 0)
        } else {
            let g = owning.div_ceil(group);
            (g, owning.saturating_sub(g))
        };

        let per_global = (k + v) * kv_heads * BYTES_PER_ELEMENT;
        let per_swa = (u64::from(k_swa) + u64::from(v_swa)) * kv_heads * BYTES_PER_ELEMENT;
        Some(u64::from(global) * per_global + u64::from(swa) * per_swa)
    }

    /// [`Self::kv_bytes_per_token`] in KiB, which is the unit the context
    /// arithmetic actually works in.
    pub fn kv_kib_per_token(&self, swa_per_global: u32) -> Option<u64> {
        self.kv_bytes_per_token(swa_per_global).map(|b| b / 1024)
    }

    /// A one-line description, in the order a person reads a model name.
    ///
    /// "Gemma3 · 4.3B · Q4_K_M · 8192 ctx". Parts that the header did not
    /// carry are simply absent rather than filled with "unknown" — a
    /// description made mostly of the word unknown is worse than a short one.
    pub fn summary(&self) -> Option<String> {
        let mut parts: Vec<String> = Vec::new();
        if let Some(a) = &self.architecture {
            parts.push(title_case(a));
        }
        if let Some(p) = self.parameter_count {
            parts.push(format_params(p));
        }
        if let Some(q) = &self.quantization {
            parts.push(q.clone());
        }
        if let Some(c) = self.context_length {
            parts.push(format!("{} ctx", format_thousands(c)));
        }
        if parts.is_empty() {
            None
        } else {
            Some(parts.join(" · "))
        }
    }
}

/// `general.file_type` → the quantisation name people recognise.
///
/// The numbering is llama.cpp's `LLAMA_FTYPE_*`. Unknown values return `None`
/// rather than a guess: a wrong quantisation label is worse than no label,
/// because it is the number people use to predict speed and quality.
fn file_type_name(v: u32) -> Option<&'static str> {
    Some(match v {
        0 => "F32",
        1 => "F16",
        2 => "Q4_0",
        3 => "Q4_1",
        7 => "Q8_0",
        8 => "Q5_0",
        9 => "Q5_1",
        10 => "Q2_K",
        11 => "Q3_K_S",
        12 => "Q3_K_M",
        13 => "Q3_K_L",
        14 => "Q4_K_S",
        15 => "Q4_K_M",
        16 => "Q5_K_S",
        17 => "Q5_K_M",
        18 => "Q6_K",
        19 => "IQ2_XXS",
        20 => "IQ2_XS",
        21 => "Q2_K_S",
        22 => "IQ3_XS",
        23 => "IQ3_XXS",
        24 => "IQ1_S",
        25 => "IQ4_NL",
        26 => "IQ3_S",
        27 => "IQ3_M",
        28 => "IQ2_S",
        29 => "IQ2_M",
        30 => "IQ4_XS",
        31 => "IQ1_M",
        32 => "BF16",
        _ => return None,
    })
}

/// A cursor that refuses to read past the end.
struct Reader<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let end = self.pos.checked_add(n)?;
        let slice = self.buf.get(self.pos..end)?;
        self.pos = end;
        Some(slice)
    }
    fn u32(&mut self) -> Option<u32> {
        Some(u32::from_le_bytes(self.take(4)?.try_into().ok()?))
    }
    fn u64(&mut self) -> Option<u64> {
        Some(u64::from_le_bytes(self.take(8)?.try_into().ok()?))
    }
    fn string(&mut self) -> Option<String> {
        let len = self.u64()? as usize;
        // A length field is 64 bits wide and comes from the file. Refusing an
        // absurd one here is what stops a corrupt header asking for a 16 EB
        // allocation.
        if len > self.buf.len() {
            return None;
        }
        Some(String::from_utf8_lossy(self.take(len)?).into_owned())
    }
}

/// Fixed widths for the scalar value types, so a value we do not care about
/// can be stepped over without interpreting it.
fn scalar_width(kind: u32) -> Option<usize> {
    Some(match kind {
        0 | 1 | 7 => 1,    // u8, i8, bool
        2 | 3 => 2,        // u16, i16
        4 | 5 | 6 => 4,    // u32, i32, f32
        10 | 11 | 12 => 8, // u64, i64, f64
        _ => return None,
    })
}

/// Read a value, returning it only when it is a kind we can use.
enum Value {
    U32(u32),
    U64(u64),
    Str(String),
    Other,
}

fn read_value(r: &mut Reader<'_>, kind: u32) -> Option<Value> {
    match kind {
        8 => Some(Value::Str(r.string()?)),
        4 => Some(Value::U32(r.u32()?)),
        10 => Some(Value::U64(r.u64()?)),
        9 => {
            // Array: element type, count, elements. Stepped over rather than
            // collected — nothing here needs one, and skipping keeps the walk
            // going so later keys are still read.
            let elem = r.u32()?;
            let count = r.u64()? as usize;
            if elem == 8 {
                for _ in 0..count {
                    r.string()?;
                }
            } else {
                let w = scalar_width(elem)?;
                r.take(w.checked_mul(count)?)?;
            }
            Some(Value::Other)
        }
        other => {
            r.take(scalar_width(other)?)?;
            Some(Value::Other)
        }
    }
}

/// Read what a GGUF header says about its model.
///
/// `head` need only be the first slice of the file — a megabyte is far more
/// than any real header. A short read simply yields fewer fields.
///
/// Returns `None` when the bytes are not GGUF at all, so a caller can tell
/// "not this kind of file" from "a header with little in it".
pub fn parse_gguf_header(head: &[u8]) -> Option<GgufInfo> {
    let mut r = Reader { buf: head, pos: 0 };
    if r.take(4)? != b"GGUF" {
        return None;
    }
    let _version = r.u32()?;
    let tensor_count = r.u64()?;
    let kv_count = r.u64()?;

    let mut info = GgufInfo {
        tensor_count: Some(tensor_count),
        ..Default::default()
    };

    // Bounded by the header's own count AND by a ceiling, so a corrupt count
    // cannot spin this loop.
    for _ in 0..kv_count.min(4096) {
        let Some(key) = r.string() else { break };
        let Some(kind) = r.u32() else { break };
        let Some(value) = read_value(&mut r, kind) else {
            break;
        };

        match (key.as_str(), value) {
            ("general.architecture", Value::Str(s)) => info.architecture = Some(s),
            ("general.name", Value::Str(s)) => info.name = Some(s),
            ("general.file_type", Value::U32(v)) => {
                info.quantization = file_type_name(v).map(str::to_string)
            }
            ("general.parameter_count", Value::U64(v)) => info.parameter_count = Some(v),
            ("general.parameter_count", Value::U32(v)) => info.parameter_count = Some(u64::from(v)),
            // Architecture-scoped keys: "gemma3.context_length",
            // "llama.block_count". Matched by suffix rather than by building
            // the prefix from `general.architecture`, because the two do not
            // always agree and the suffix is unambiguous either way.
            (k, Value::U32(v)) if k.ends_with(".context_length") => info.context_length = Some(v),
            (k, Value::U32(v)) if k.ends_with(".embedding_length") => {
                info.embedding_length = Some(v)
            }
            (k, Value::U32(v)) if k.ends_with(".block_count") => info.block_count = Some(v),
            (k, Value::U32(v)) if k.ends_with(".attention.head_count_kv") => {
                info.head_count_kv = Some(v)
            }
            // `_swa` first: ".attention.key_length_swa" also ends with
            // nothing else, but ".key_length" is a suffix-match that would
            // never fire for it -- kept explicit so a reader does not have to
            // work that out.
            (k, Value::U32(v)) if k.ends_with(".attention.key_length_swa") => {
                info.key_length_swa = Some(v)
            }
            (k, Value::U32(v)) if k.ends_with(".attention.value_length_swa") => {
                info.value_length_swa = Some(v)
            }
            (k, Value::U32(v)) if k.ends_with(".attention.key_length") => info.key_length = Some(v),
            (k, Value::U32(v)) if k.ends_with(".attention.value_length") => {
                info.value_length = Some(v)
            }
            (k, Value::U32(v)) if k.ends_with(".attention.shared_kv_layers") => {
                info.shared_kv_layers = Some(v)
            }
            _ => {}
        }
    }

    Some(info)
}

/// "4.3B", "270M" — parameter counts as they are spoken.
fn format_params(n: u64) -> String {
    if n >= 1_000_000_000 {
        let b = n as f64 / 1_000_000_000.0;
        if b >= 100.0 {
            format!("{b:.0}B")
        } else {
            format!("{b:.1}B")
        }
    } else if n >= 1_000_000 {
        format!("{}M", n / 1_000_000)
    } else {
        n.to_string()
    }
}

/// 131072 → "131,072". Context windows are long enough that the grouping is
/// what makes them readable at a glance.
fn format_thousands(n: u32) -> String {
    let s = n.to_string();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// "gemma3" → "Gemma3". Only the first letter: the rest of an architecture
/// string is often deliberately cased ("qwen2moe").
fn title_case(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
        None => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The two Gemma 4 models as their headers describe them, read off the
    /// real files on 2026-08-16 (`gemma4.attention.*`, offsets 924-1,829).
    fn gemma_e2b() -> GgufInfo {
        GgufInfo {
            architecture: Some("gemma4".into()),
            block_count: Some(35),
            head_count_kv: Some(1),
            key_length: Some(512),
            value_length: Some(512),
            key_length_swa: Some(256),
            value_length_swa: Some(256),
            shared_kv_layers: Some(20),
            ..Default::default()
        }
    }

    fn gemma_e4b() -> GgufInfo {
        GgufInfo {
            architecture: Some("gemma4".into()),
            block_count: Some(42),
            head_count_kv: Some(2),
            key_length: Some(512),
            value_length: Some(512),
            key_length_swa: Some(256),
            value_length_swa: Some(256),
            shared_kv_layers: Some(18),
            ..Default::default()
        }
    }

    /// The claim this whole function exists to make: the header alone
    /// reproduces what the device measured, so the constant in
    /// `jetson_context_size` does not have to be measured per model.
    ///
    /// Measured on the Orin 2026-08-12 by reading llama.cpp's own
    /// `llama_kv_cache ... size = N MiB (C cells, L layers)` lines:
    /// E2B 96 + 192 MiB at n_ctx 16384 = 18 KiB/token; E4B 128 + 320 MiB at
    /// n_ctx 8192 = 56 KiB/token. Both caches carry `n_ctx` cells, so the cost
    /// is linear with no constant term.
    #[test]
    fn kv_cost_reproduces_the_device_measurements() {
        assert_eq!(
            gemma_e2b().kv_kib_per_token(5),
            Some(18),
            "E2B: 15 owning layers (35 - 20), 3 global at 2 KiB + 12 SWA at 1 KiB"
        );
        assert_eq!(
            gemma_e4b().kv_kib_per_token(5),
            Some(56),
            "E4B: 24 owning layers (42 - 18), 4 global at 4 KiB + 20 SWA at 2 KiB"
        );
    }

    /// The same claim, against real files rather than transcribed numbers.
    ///
    /// `#[ignore]` and env-gated, following the convention the local-inference
    /// crate uses: the GGUFs are gigabytes and are not in the repo. Run with
    ///
    /// ```text
    /// GIAP_TEST_GGUF_DIR="$HOME/Library/Application Support/goose-in-a-pond/models/gguf" \
    ///   cargo test -p pond-core --lib gguf -- --ignored --nocapture
    /// ```
    ///
    /// Transcribing header values into a fixture and asserting on the
    /// transcription proves the arithmetic, not the reading. This proves both.
    #[test]
    #[ignore = "needs real GGUF files; set GIAP_TEST_GGUF_DIR"]
    fn kv_cost_from_the_real_files_on_disk() {
        let Ok(dir) = std::env::var("GIAP_TEST_GGUF_DIR") else {
            eprintln!("GIAP_TEST_GGUF_DIR unset");
            return;
        };
        // Geometry lives in the first ~2 KB, long before the token array.
        const HEAD: usize = 64 * 1024;
        let expected = [
            ("gemma-4-E2B-it-Q4_K_M.gguf", 18u64),
            ("gemma-4-E4B-it-Q4_K_M.gguf", 56),
        ];

        let mut checked = 0;
        for (file, want) in expected {
            let path = std::path::Path::new(&dir).join(file);
            let Ok(bytes) = std::fs::read(&path) else {
                eprintln!("skip (absent): {}", path.display());
                continue;
            };
            let head = &bytes[..bytes.len().min(HEAD)];
            let info = parse_gguf_header(head).expect("real GGUF should parse");
            let got = info.kv_kib_per_token(5);
            eprintln!(
                "{file}: blocks={:?} shared={:?} kv_heads={:?} k={:?} k_swa={:?} -> {got:?} KiB/token",
                info.block_count, info.shared_kv_layers, info.head_count_kv,
                info.key_length, info.key_length_swa
            );
            assert_eq!(got, Some(want), "{file} KV cost");
            checked += 1;
        }
        assert!(checked > 0, "no model files found under {dir}");
    }

    /// Shared layers allocate nothing, and forgetting that is a 1.75x
    /// overestimate on E4B -- which reads as "this model does not fit" and
    /// silently costs context.
    #[test]
    fn shared_layers_allocate_no_cache() {
        let mut all_owning = gemma_e4b();
        all_owning.shared_kv_layers = None;
        let shared = gemma_e4b().kv_bytes_per_token(5).expect("computed");
        let unshared = all_owning.kv_bytes_per_token(5).expect("computed");
        assert!(
            unshared > shared,
            "ignoring shared_kv_layers must cost more, got {unshared} vs {shared}"
        );
    }

    /// An architecture with no sliding window pays full width on every layer.
    /// This is the conservative direction, which is the right one to be wrong in.
    #[test]
    fn no_swa_widths_means_full_width_everywhere() {
        let dense = GgufInfo {
            block_count: Some(28),
            head_count_kv: Some(2),
            key_length: Some(128),
            value_length: Some(128),
            ..Default::default()
        };
        // 28 layers x (128+128) x 2 heads x 2 bytes = 28,672 bytes = 28 KiB.
        assert_eq!(dense.kv_kib_per_token(5), Some(28));
    }

    /// A header missing what the sum needs must yield nothing, so the caller
    /// keeps its measured fallback instead of acting on a confident guess.
    /// This is the constant that can OOM a board.
    #[test]
    fn incomplete_headers_refuse_rather_than_guess() {
        assert_eq!(GgufInfo::default().kv_kib_per_token(5), None);

        let no_heads = GgufInfo {
            block_count: Some(35),
            key_length: Some(512),
            value_length: Some(512),
            ..Default::default()
        };
        assert_eq!(no_heads.kv_kib_per_token(5), None);

        let zero_heads = GgufInfo {
            block_count: Some(35),
            head_count_kv: Some(0),
            key_length: Some(512),
            value_length: Some(512),
            ..Default::default()
        };
        assert_eq!(zero_heads.kv_kib_per_token(5), None);
    }

    use super::*;

    /// Build a GGUF header the way a writer would, so the parser is tested
    /// against the format rather than against itself.
    struct HeaderBuilder {
        kvs: Vec<u8>,
        count: u64,
    }

    impl HeaderBuilder {
        fn new() -> Self {
            Self {
                kvs: Vec::new(),
                count: 0,
            }
        }
        fn raw_string(out: &mut Vec<u8>, s: &str) {
            out.extend_from_slice(&(s.len() as u64).to_le_bytes());
            out.extend_from_slice(s.as_bytes());
        }
        fn str_kv(mut self, k: &str, v: &str) -> Self {
            Self::raw_string(&mut self.kvs, k);
            self.kvs.extend_from_slice(&8u32.to_le_bytes());
            Self::raw_string(&mut self.kvs, v);
            self.count += 1;
            self
        }
        fn u32_kv(mut self, k: &str, v: u32) -> Self {
            Self::raw_string(&mut self.kvs, k);
            self.kvs.extend_from_slice(&4u32.to_le_bytes());
            self.kvs.extend_from_slice(&v.to_le_bytes());
            self.count += 1;
            self
        }
        fn u64_kv(mut self, k: &str, v: u64) -> Self {
            Self::raw_string(&mut self.kvs, k);
            self.kvs.extend_from_slice(&10u32.to_le_bytes());
            self.kvs.extend_from_slice(&v.to_le_bytes());
            self.count += 1;
            self
        }
        /// A string array — the shape `tokenizer.ggml.tokens` takes, and the
        /// one that has to be stepped over rather than read.
        fn str_array_kv(mut self, k: &str, items: &[&str]) -> Self {
            Self::raw_string(&mut self.kvs, k);
            self.kvs.extend_from_slice(&9u32.to_le_bytes());
            self.kvs.extend_from_slice(&8u32.to_le_bytes());
            self.kvs
                .extend_from_slice(&(items.len() as u64).to_le_bytes());
            for it in items {
                Self::raw_string(&mut self.kvs, it);
            }
            self.count += 1;
            self
        }
        fn build(self) -> Vec<u8> {
            let mut out = Vec::new();
            out.extend_from_slice(b"GGUF");
            out.extend_from_slice(&3u32.to_le_bytes());
            out.extend_from_slice(&291u64.to_le_bytes()); // tensor_count
            out.extend_from_slice(&self.count.to_le_bytes());
            out.extend_from_slice(&self.kvs);
            out
        }
    }

    fn gemma_header() -> Vec<u8> {
        HeaderBuilder::new()
            .str_kv("general.architecture", "gemma3")
            .str_kv("general.name", "Gemma 3 4B It")
            .u32_kv("general.file_type", 15) // Q4_K_M
            .u64_kv("general.parameter_count", 4_300_000_000)
            .u32_kv("gemma3.context_length", 8192)
            .u32_kv("gemma3.embedding_length", 2560)
            .u32_kv("gemma3.block_count", 34)
            .build()
    }

    #[test]
    fn reads_what_the_file_says_about_itself() {
        let info = parse_gguf_header(&gemma_header()).expect("is gguf");
        assert_eq!(info.architecture.as_deref(), Some("gemma3"));
        assert_eq!(info.name.as_deref(), Some("Gemma 3 4B It"));
        assert_eq!(info.quantization.as_deref(), Some("Q4_K_M"));
        assert_eq!(info.parameter_count, Some(4_300_000_000));
        assert_eq!(info.context_length, Some(8192));
        assert_eq!(info.embedding_length, Some(2560));
        assert_eq!(info.block_count, Some(34));
        assert_eq!(info.tensor_count, Some(291));
    }

    #[test]
    fn summarises_in_the_order_a_model_name_is_read() {
        let info = parse_gguf_header(&gemma_header()).unwrap();
        assert_eq!(
            info.summary().as_deref(),
            Some("Gemma3 · 4.3B · Q4_K_M · 8,192 ctx")
        );
    }

    /// Real headers carry a token vocabulary — tens of thousands of strings
    /// between the keys we want. Stepping over it has to work or everything
    /// after it is lost.
    #[test]
    fn steps_over_the_arrays_it_does_not_need() {
        let bytes = HeaderBuilder::new()
            .str_kv("general.architecture", "llama")
            .str_array_kv("tokenizer.ggml.tokens", &["<s>", "</s>", "hello", "world"])
            .u32_kv("llama.context_length", 131072)
            .build();

        let info = parse_gguf_header(&bytes).expect("is gguf");
        assert_eq!(info.architecture.as_deref(), Some("llama"));
        // The key AFTER the array is what proves the skip landed correctly.
        assert_eq!(info.context_length, Some(131072));
    }

    #[test]
    fn matches_architecture_scoped_keys_whatever_the_prefix() {
        for arch in ["llama", "qwen3", "phi3", "gemma3"] {
            let bytes = HeaderBuilder::new()
                .u32_kv(&format!("{arch}.context_length"), 4096)
                .build();
            assert_eq!(
                parse_gguf_header(&bytes).unwrap().context_length,
                Some(4096)
            );
        }
    }

    #[test]
    fn says_when_the_bytes_are_not_gguf_at_all() {
        assert!(parse_gguf_header(b"ONNX....").is_none());
        assert!(parse_gguf_header(b"").is_none());
        assert!(parse_gguf_header(b"GGU").is_none());
    }

    /// A model file is arbitrary bytes from the internet. The worst a broken
    /// header should cost is a card with less on it.
    #[test]
    fn survives_a_header_cut_off_mid_field() {
        let full = gemma_header();
        for cut in 0..full.len() {
            let _ = parse_gguf_header(&full[..cut]); // must not panic
        }
        // Truncated right after the counts: valid GGUF, nothing learned.
        let info = parse_gguf_header(&full[..24]).expect("still gguf");
        assert_eq!(info.architecture, None);
        assert_eq!(info.tensor_count, Some(291));
    }

    #[test]
    fn refuses_an_absurd_string_length_rather_than_allocating_it() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"GGUF");
        bytes.extend_from_slice(&3u32.to_le_bytes());
        bytes.extend_from_slice(&0u64.to_le_bytes());
        bytes.extend_from_slice(&1u64.to_le_bytes());
        // A key claiming to be 16 exabytes long.
        bytes.extend_from_slice(&u64::MAX.to_le_bytes());

        let info = parse_gguf_header(&bytes).expect("header itself is valid");
        assert!(info.architecture.is_none());
    }

    #[test]
    fn a_corrupt_kv_count_cannot_spin_the_walk() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"GGUF");
        bytes.extend_from_slice(&3u32.to_le_bytes());
        bytes.extend_from_slice(&0u64.to_le_bytes());
        bytes.extend_from_slice(&u64::MAX.to_le_bytes()); // kv_count lies
        assert!(parse_gguf_header(&bytes).is_some());
    }

    #[test]
    fn leaves_an_unrecognised_quantisation_unnamed() {
        let bytes = HeaderBuilder::new()
            .u32_kv("general.file_type", 9999)
            .build();
        assert_eq!(parse_gguf_header(&bytes).unwrap().quantization, None);
    }

    #[test]
    fn spells_parameter_counts_and_windows_the_way_they_are_spoken() {
        assert_eq!(format_params(4_300_000_000), "4.3B");
        assert_eq!(format_params(270_000_000), "270M");
        assert_eq!(format_params(120_000_000_000), "120B");
        assert_eq!(format_thousands(131072), "131,072");
        assert_eq!(format_thousands(8192), "8,192");
        assert_eq!(format_thousands(512), "512");
    }

    #[test]
    fn a_header_with_nothing_useful_says_so() {
        let info = parse_gguf_header(&HeaderBuilder::new().build()).unwrap();
        assert!(info.summary().is_none());
    }
}
