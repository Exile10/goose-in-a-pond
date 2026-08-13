//! GIAP must never enter `LlamaBackend::init()`'s process-global CAS.
//!
//! `llama-cpp-2` tracks backend initialisation in one process-wide `AtomicBool`,
//! and cargo unifies that crate between this workspace and Goose, so the static
//! is shared. Goose treats losing the CAS as `unreachable!` and PANICS
//! (`goose-local-inference/src/llamacpp/mod.rs`). Reproduced on a Mac
//! 2026-08-13: an `ollama` pond embedded at startup, GIAP won the CAS, and the
//! next local chat model panicked a tokio worker, after which the API stopped
//! answering.
//!
//! The fix is structural: `engine.rs :: get_or_init_backend` calls
//! `llama_cpp_sys_2::llama_backend_init()` directly and constructs the
//! proof-of-init token itself, so the flag is only ever set by Goose and its CAS
//! always succeeds. These tests exist because that property is invisible at the
//! call site -- `LlamaBackend::init()` is the obvious, documented, wrong thing to
//! reach for, and nothing but this test would notice it coming back.
//!
//! Source-scanning rather than behavioural on purpose: the failure needs Goose
//! and a real model in one process, which no unit test in this crate can build.

use std::path::Path;

/// Every `.rs` file in this crate, as (display path, contents).
fn crate_sources() -> Vec<(String, String)> {
    fn walk(dir: &Path, out: &mut Vec<(String, String)>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(&path, out);
            } else if path.extension().is_some_and(|e| e == "rs") {
                if let Ok(text) = std::fs::read_to_string(&path) {
                    out.push((path.display().to_string(), text));
                }
            }
        }
    }
    let mut out = Vec::new();
    walk(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("src").as_path(),
        &mut out,
    );
    assert!(
        !out.is_empty(),
        "the source scan found nothing -- walker is broken"
    );
    out
}

/// Strip `//` line comments so the prose in `get_or_init_backend`'s doc comment
/// -- which necessarily NAMES `LlamaBackend::init()` to explain why it is not
/// called -- does not trip the scan. Without this the guard fires on its own
/// rationale, which is how a tripwire gets deleted instead of heeded.
fn strip_line_comments(src: &str) -> String {
    src.lines()
        .map(|l| match l.find("//") {
            Some(i) => &l[..i],
            None => l,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn no_giap_code_calls_llama_backend_init() {
    let mut offenders = Vec::new();
    for (path, src) in crate_sources() {
        let code = strip_line_comments(&src);
        for (lineno, line) in code.lines().enumerate() {
            if line.contains("LlamaBackend::init") {
                offenders.push(format!("{path}:{}", lineno + 1));
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "GIAP called LlamaBackend::init() at {offenders:?}.\n\
         That enters llama-cpp-2's process-global CAS, which Goose assumes it \
         always wins -- losing it makes Goose hit `unreachable!` and panic the \
         moment a local chat model loads. Initialise the C backend directly \
         instead (see engine.rs :: get_or_init_backend) and let Goose own the flag."
    );
}

/// The token must never be dropped: `impl Drop for LlamaBackend` resets the
/// global flag AND calls `llama_backend_free()`, which in a two-consumer process
/// frees the backend under the other consumer and makes the second dropper panic
/// inside a destructor. Holding a strong `Arc` in a `OnceLock` for the life of
/// the process is what prevents that, so a `Weak` here is a regression.
#[test]
fn the_backend_handle_is_held_strongly_and_never_freed() {
    let engine = crate_sources()
        .into_iter()
        .find(|(p, _)| p.ends_with("engine.rs"))
        .map(|(_, s)| strip_line_comments(&s))
        .expect("engine.rs not found");

    assert!(
        engine.contains("static BACKEND: OnceLock<Arc<LlamaBackend>>"),
        "the shared backend must be held as a strong Arc in a OnceLock. A Weak \
         lets the last engine drop it, which runs Drop -- resetting a flag GIAP \
         does not own and calling llama_backend_free() under Goose."
    );
    assert!(
        !engine.contains("Weak<LlamaBackend>"),
        "a Weak<LlamaBackend> is back: the backend can be freed while Goose still \
         holds it. Once the backend is shared the only sound rule is initialise \
         once, never free."
    );
}
