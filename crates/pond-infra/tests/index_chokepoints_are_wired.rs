//! Phase B: does *production* mirror its memory writes into the shared index?
//!
//! `SqliteMemoryRepository::with_vector_index` is documented as a 100%
//! chokepoint -- nothing else in the workspace issues SQL against
//! `memory_fragments`, so `add`, `delete` and `update_embedding` see every
//! write. That is true of the adapter and says nothing about `main.rs`, which is
//! where the chokepoint was actually leaking: the type has FOUR production
//! construction sites and for a long while exactly one of them chained the
//! index.
//!
//! The three that did not were `run_chat` (the voice/CLI path, which hands its
//! repo to `build_goose_backend` and so registers the `giap-memory` tool),
//! `run_agent_cmd` (same, for all three of its arms), and `run_memories_cmd`
//! (`pond memories add` / `remove`, a separate process with its own
//! `Database`). Every one of them compiles, runs, and looks correct. The symptom
//! is not an error: it is a memory that is present in `pond_system.db` and
//! absent from `pond_vectors.db` until a sweep happens to notice, and a `remove`
//! that takes the row and leaves the vector behind as an orphan. Nothing warned
//! you, unlike redaction -- which had a guard, and which is why the redaction
//! sites stayed wired while these three did not.
//!
//! Like its sibling `redaction_chokepoints_are_wired.rs`, this reads `main.rs`
//! as text, which is a weaker instrument than an integration test and is chosen
//! for the same reason: `pond-server` is not in `ci.yml`'s test list (it is
//! covered only by `cargo check`), so a guard that lives there never runs on a
//! pull request. `pond-infra` owns both `SqliteMemoryRepository` and
//! `SqliteVectorIndex` -- the two adapters these bindings exist to join -- and it
//! *is* in that list. `include_str!` creates no dependency edge: it does not
//! link `pond-server`, it just refuses to compile if the path moves.
//!
//! # Why this scans DOWN and its sibling scans UP
//!
//! The one line worth being careful about. `RedactingMemoryRepository::new(` is
//! a decorator: it appears ABOVE the construction it wraps, so the redaction
//! guard looks upward. `.with_vector_index(` is a builder method: it appears
//! ON or BELOW the construction it modifies. Copying the sibling's
//! `wrapped_within` verbatim would have produced a guard that never found a
//! single chained site and therefore failed loudly -- the harmless failure. The
//! dangerous one is the mirror image of the bug that guard already carries a
//! fixture for, and it is pinned below.

const MAIN: &str = include_str!("../../pond-server/src/main.rs");

/// Sites that legitimately do NOT chain the index, keyed by the enclosing
/// function, each with the reason it is exempt.
///
/// Empty, deliberately, and kept as machinery rather than deleted: every one of
/// the four paths has a `Database` and therefore a `db.vectors` pool, so "no
/// index available here" is not a reason any current site can claim. What three
/// of them genuinely lack is an EMBEDDER, and that is a reason to pass a `None`
/// model id -- not a reason to skip the index. `the_cli_paths_index_without_a_model_id`
/// below records that distinction by name, because collapsing the two is how a
/// path would quietly stop mirroring while looking like it had a good excuse.
///
/// A future entry belongs here only if the site truly cannot reach a vector
/// pool. Anything else is a bypass wearing an exemption.
const EXCLUSIONS: &[(&str, &str)] = &[];

/// The builder call must appear on the construction's own line or in the few
/// lines below it -- `rustfmt` splits `.with_vector_index(` off onto its own
/// line whenever the arguments are long, and three of the four production sites
/// keep it inline, so neither a single-line nor a next-line-only check works.
///
/// `construction` bounds the search: the window must never reach FORWARD over a
/// LATER construction of the same type. That is the same defect the sibling
/// guard documents in reverse -- there, a bare construction below a wrapped one
/// inherited the wrapper; here, a bare construction ABOVE a chained one would
/// find the chained one's `.with_vector_index(` inside its own window and be
/// reported as indexed. Pinned as a fixture in
/// `the_window_never_reaches_over_a_later_construction`.
fn chained_within(lines: &[&str], site: usize, method: &str, construction: &str) -> bool {
    let ceiling = (site + 7).min(lines.len());
    let mut end = ceiling;
    for (i, line) in lines.iter().enumerate().take(ceiling).skip(site + 1) {
        if line.contains(construction) {
            end = i;
            break;
        }
    }
    lines[site..end].iter().any(|l| l.contains(method))
}

fn sites(lines: &[&str], needle: &str) -> Vec<usize> {
    lines
        .iter()
        .enumerate()
        .filter(|(_, l)| l.contains(needle))
        .map(|(i, _)| i)
        .collect()
}

/// Which top-level function a line sits in, by scanning up to the nearest
/// column-zero `fn`. Used so a failure names the path a reader can go and look
/// at -- `main.rs:8457` alone sends them hunting, `run_memories_cmd` does not.
fn enclosing_fn(lines: &[&str], site: usize) -> String {
    for line in lines[..=site].iter().rev() {
        for prefix in ["async fn ", "fn ", "pub async fn ", "pub fn "] {
            if let Some(rest) = line.strip_prefix(prefix) {
                let name: String = rest
                    .chars()
                    .take_while(|c| c.is_alphanumeric() || *c == '_')
                    .collect();
                if !name.is_empty() {
                    return name;
                }
            }
        }
    }
    "<unknown>".to_string()
}

#[test]
fn every_memory_repository_construction_mirrors_into_the_index() {
    let lines: Vec<&str> = MAIN.lines().collect();
    let found = sites(&lines, "SqliteMemoryRepository::new(");
    // Pinned, not a floor -- the lesson the sibling guard learned the expensive
    // way. `>= 4` lets a FIFTH construction appear unnoticed, which is the
    // likeliest shape for a new bypass: a write path added beside an existing
    // one, copied from whichever neighbour was closest.
    assert_eq!(
        found.len(),
        4,
        "found {} SqliteMemoryRepository::new( sites in main.rs; there were 4 \
         (run_server, run_chat, run_agent_cmd, run_memories_cmd). A new one is a \
         new write path and needs the index chained; a missing one means this \
         guard has stopped matching and is asserting nothing.",
        found.len()
    );

    let where_they_are: Vec<String> = found.iter().map(|&s| enclosing_fn(&lines, s)).collect();
    assert_eq!(
        where_they_are,
        vec![
            "run_server",
            "run_chat",
            "run_agent_cmd",
            "run_memories_cmd"
        ],
        "the four constructions are no longer in the four functions this guard \
         reasons about. Either a path moved or a new one appeared; re-read the \
         module comment before adjusting this list, because the exemption \
         reasons below are written against these specific paths."
    );

    for (site, function) in found.iter().zip(&where_they_are) {
        if let Some((_, reason)) = EXCLUSIONS.iter().find(|(f, _)| f == function) {
            // An exemption is a claim about the path, so make it cost something:
            // if the site starts chaining the index anyway, the claim is stale
            // and the entry should go rather than sit there excusing nothing.
            assert!(
                !chained_within(
                    &lines,
                    *site,
                    ".with_vector_index(",
                    "SqliteMemoryRepository::new("
                ),
                "{function} is listed in EXCLUSIONS ({reason}) but now chains \
                 .with_vector_index anyway. Remove the exemption -- a stale one \
                 will excuse the next real bypass on this path."
            );
            continue;
        }
        assert!(
            chained_within(
                &lines,
                *site,
                ".with_vector_index(",
                "SqliteMemoryRepository::new("
            ),
            "main.rs:{} constructs SqliteMemoryRepository in {function} without \
             chaining .with_vector_index. Every memory that path writes lands in \
             pond_system.db and never reaches pond_vectors.db, and every memory \
             it deletes leaves its vector behind as an orphan -- both invisible \
             until semantic recall silently misses a fact the member is certain \
             they said. Chain the index, or if this path genuinely cannot reach \
             a vector pool, add it to EXCLUSIONS with the reason.",
            site + 1
        );
    }
}

/// The reason three of the four sites pass `None` for the model id, recorded so
/// that a later reader does not "fix" it by inventing one.
///
/// `run_chat`, `run_agent_cmd` and `run_memories_cmd` have no embedder --
/// the first two pass `None` for `embedding_provider` into
/// `build_goose_backend`, and the third is a bare CLI process. A vector they
/// could not attribute to a model is exactly the case `mirror` handles by
/// returning: it leaves any existing entry alone and lets the sweep own it,
/// rather than stripping entries the server had correctly written. Passing a
/// guessed model id there would corrupt the index far more quietly than not
/// indexing at all, which is why this is pinned rather than left to taste.
#[test]
fn the_cli_paths_index_without_a_model_id() {
    let lines: Vec<&str> = MAIN.lines().collect();
    let found = sites(&lines, "SqliteMemoryRepository::new(");

    let attributed: Vec<String> = found
        .iter()
        .filter(|&&site| {
            chained_within(
                &lines,
                site,
                "vector_model_id",
                "SqliteMemoryRepository::new(",
            )
        })
        .map(|&site| enclosing_fn(&lines, site))
        .collect();

    assert_eq!(
        attributed,
        vec!["run_server"],
        "exactly one path -- run_server -- constructs an embedding provider and \
         can therefore say which model produced a vector. If a CLI path has \
         started passing a model id, check it actually built an embedder: \
         attributing vectors to a model that did not produce them makes the \
         index disagree with the store in a way no sweep repairs. If run_server \
         has stopped passing one, its writes are no longer attributable and the \
         sweep now owns work it used to do inline."
    );
}

/// The mirror image of the bug the sibling guard carries a fixture for, pinned
/// before it can happen rather than after.
///
/// `chained_within` scans a fixed window DOWNWARD, so a bare construction
/// sitting a few lines ABOVE a chained one would find the chained one's
/// `.with_vector_index(` inside its own window and be reported as indexed. The
/// sibling guard's version of this went unnoticed until a review demonstrated it
/// by hand; there is no reason to re-learn it here.
///
/// Pinned on synthetic lines, because the alternative is editing `main.rs` in
/// place and asserting the suite goes red -- which proves it once, leaves
/// nothing behind, and cannot run in CI.
#[test]
fn the_window_never_reaches_over_a_later_construction() {
    let lines = vec![
        "    let leaky = Arc::new(SqliteMemoryRepository::new(db.system.clone()));",
        "",
        "    let split = Arc::new(",
        "        SqliteMemoryRepository::new(db.system.clone())",
        "            .with_vector_index(vector_index.clone(), vector_model_id.clone()),",
        "    );",
        "    let inline = Arc::new(SqliteMemoryRepository::new(p).with_vector_index(ix, None));",
    ];
    let found = sites(&lines, "SqliteMemoryRepository::new(");
    assert_eq!(found, vec![0, 3, 6], "fixture must contain all three forms");

    assert!(
        !chained_within(
            &lines,
            0,
            ".with_vector_index(",
            "SqliteMemoryRepository::new("
        ),
        "a bare construction ABOVE a chained one must NOT inherit its builder \
         call -- this is the whole failure mode the window bound exists for"
    );
    // Both of these must still read as chained, or the guard could pass by
    // rejecting everything, which is the other way a text guard goes vacuous.
    assert!(
        chained_within(
            &lines,
            3,
            ".with_vector_index(",
            "SqliteMemoryRepository::new("
        ),
        "a construction whose builder call rustfmt split onto the next line must \
         still read as chained"
    );
    assert!(
        chained_within(
            &lines,
            6,
            ".with_vector_index(",
            "SqliteMemoryRepository::new("
        ),
        "a construction chained on its own line must still read as chained -- \
         three of the four production sites are written this way"
    );
}

/// `enclosing_fn` is load-bearing twice over: it names the path in every failure
/// message and it is the key `EXCLUSIONS` is looked up by. A silent regression to
/// `<unknown>` would make an exemption stop matching and, worse, make the
/// four-function assertion above fail for a reason that reads like a code change.
#[test]
fn enclosing_fn_finds_the_nearest_column_zero_function() {
    let lines = vec![
        "async fn run_server(",
        "    port: u16,",
        ") -> Result<()> {",
        "    let repo = SqliteMemoryRepository::new(db.system.clone());",
        "}",
        "",
        "async fn run_memories_cmd(action: MemoryAction) -> Result<()> {",
        "    let repo = SqliteMemoryRepository::new(db.system.clone());",
    ];
    assert_eq!(enclosing_fn(&lines, 3), "run_server");
    assert_eq!(
        enclosing_fn(&lines, 7),
        "run_memories_cmd",
        "the scan must stop at the NEAREST enclosing fn, not the first one in \
         the file"
    );
}
