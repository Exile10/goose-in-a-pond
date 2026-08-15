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
}

impl GgufInfo {
    /// Did the header yield anything worth showing?
    pub fn is_empty(&self) -> bool {
        *self == GgufInfo::default()
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
        0 | 1 | 7 => 1,      // u8, i8, bool
        2 | 3 => 2,          // u16, i16
        4 | 5 | 6 => 4,      // u32, i32, f32
        10 | 11 | 12 => 8,   // u64, i64, f64
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
            ("general.parameter_count", Value::U32(v)) => {
                info.parameter_count = Some(u64::from(v))
            }
            // Architecture-scoped keys: "gemma3.context_length",
            // "llama.block_count". Matched by suffix rather than by building
            // the prefix from `general.architecture`, because the two do not
            // always agree and the suffix is unambiguous either way.
            (k, Value::U32(v)) if k.ends_with(".context_length") => {
                info.context_length = Some(v)
            }
            (k, Value::U32(v)) if k.ends_with(".embedding_length") => {
                info.embedding_length = Some(v)
            }
            (k, Value::U32(v)) if k.ends_with(".block_count") => info.block_count = Some(v),
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

    /// Build a GGUF header the way a writer would, so the parser is tested
    /// against the format rather than against itself.
    struct HeaderBuilder {
        kvs: Vec<u8>,
        count: u64,
    }

    impl HeaderBuilder {
        fn new() -> Self {
            Self { kvs: Vec::new(), count: 0 }
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
            self.kvs.extend_from_slice(&(items.len() as u64).to_le_bytes());
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
        assert_eq!(info.summary().as_deref(), Some("Gemma3 · 4.3B · Q4_K_M · 8,192 ctx"));
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
            assert_eq!(parse_gguf_header(&bytes).unwrap().context_length, Some(4096));
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
        let bytes = HeaderBuilder::new().u32_kv("general.file_type", 9999).build();
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
