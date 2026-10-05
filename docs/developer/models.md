# Models: engines, sources, picks and add-ons

How the pond describes and acquires conversation models. The rule that shapes all of it:
**nothing is downloaded or switched on without a person choosing it.** The only automatic
download is the boot restore of a model the household itself assigned, and it shows on the
Models page like any other download.

## Engines

Every conversation row names the engine that runs it, derived from its category
(`crates/pond-core/src/models/domain/engine.rs`; nothing is stored):

| Engine | Category | File | Runs |
|---|---|---|---|
| llama.cpp | `gguf` | `.gguf` | in the pond's process; can read pictures with an add-on |
| LiteRT-LM | `litert` | `.litertlm` | in the pond's process, on the GPU; text only |
| Ollama | `ollama` | none of the pond's | in the Ollama server |
| llamafile | `llamafile` | `.llamafile` | in its own child process |

`Engine::backend_id` is the goose registry's id (`litert` for LiteRT-LM, none for llama.cpp);
goose keeps its own copy private.

## Sources, purpose, acquisition

`crates/pond-core/src/models/domain/taxonomy.rs`, all derived from the row:

- **Provenance**: `catalogue` (one of GIAP's picks, or a bundled speech, voice or embedding row),
  `ollama` (listed by the Ollama server), `added` (has a download URL), `on_disk` (found by the
  scan). A pick that leaves the list but is still downloaded reads `added`.
- **Kind**: `conversation` or `helper`. FunctionGemma, `tool`- and `draft`-role rows, encoders,
  drafters (`mtp-*`, `dflash-*`, Gemma 4 `-assistant`) and speech or embedding files are helpers
  and are never offered as conversation. The disk scan skips companion files and helper GGUF
  architectures (`clip`, `dflash`, `*-assistant`, ASR, embeddings) altogether. An Ollama tag
  names no file, so only its words (asr, whisper, embed, functiongemma) make it a helper.
- **Acquire**: `download`, `external` (Ollama, HTTP voices, self-fetching embeddings) or
  `unavailable` (a file with no source).
- **Title**: a pick's own title; else, for a llama.cpp file the pairing table lists, the table's
  name for it ("SmolVLM 256M"); else a found file's header name; else the file's stem.

## The curated list

`crates/pond-core/src/models/domain/curated.rs` is GIAP's short list. The catalogue's
conversation rows are exactly these; anything else is added from Hugging Face or found on disk.
Names are the file stems (GGUF) and file names (LiteRT-LM) ponds already assign, so an
assignment survives a change to the list.

`recommended.rs` names three of them, with a reason each and numbers only where they were
measured on that class of machine. Only the REST view reads it
(`crates/pond-core/tests/recommendations_are_never_imposed.rs`).

### Pinning a new pick

1. Read the file's commit and LFS oid: `curl -s https://huggingface.co/api/models/<repo>/tree/<rev>`.
   The pin's `sha256` is `lfs.oid`, **never `xetHash`**. pond-hf-cache names an unpinned blob by
   the download's final ETag, which for Xet-backed files is the xet hash, so a blob's file name
   is not its content hash.
2. Prefer the commit the pick's picture add-on is already pinned at, when the file's oid there is
   the one you verified.
3. Add a `CuratedModel` with `size_bytes`, `sha256`, `context_length` (the model card's
   longest), a short `title`, and `pictures` set to the add-on's pairing `dir` (llama.cpp only).
   The pick's (repo, file) must be listed in the pairing table: a test checks every llama.cpp
   pick has its declared pairing.
4. `cargo test -p pond-core --lib -- curated recommended vision_pairing`, and the catalogue tests
   in `composite_model_catalog_provider.rs`.

## Picture add-ons: the pairing table

`crates/pond-core/data/vision-pairings.jsonl` lists every known model that needs a separate
vision encoder, compiled in and parsed once (`vision_pairing.rs`). It is generated from Hugging
Face by `bash scripts/giap.sh models pairings` and refreshed weekly through a reviewed pull
request; the pond never fetches it. A model with no line reads text only.

A model pairs when its file's base name is listed under any line, or failing that when its GGUF
header's architecture and width match a line's (with the qat flag from the name). When several
publishers list one file name, the generator's publisher preference (unsloth, ggml-org,
bartowski, lmstudio-community) decides, the same way everywhere, so a download and the model's
later use agree. On a budgeted device an add-on is offered only if its `dir` is in
`device_budget::DEVICE_MEASURED_VISION`, which is empty: the Orin reads no pictures.

## Acquisition

`crates/pond-api/src/model_acquisition.rs` plans a download before anything starts and says what
it will fetch:

- `POST /api/v1/models/{category}/{name}/download`: the model file (from its pin when it is a
  pick) and its picture add-on, included by default; `{"pictures": false}` leaves it out.
- `POST /api/v1/models/download/url`: the same planner for a file named by URL; it becomes an
  `added` row under a sanitised file name.
- `POST /api/v1/models/{category}/{name}/companions/pictures`: the add-on alone, for an installed
  model.

A model file whose size nothing has stated (a file named by URL, typically) is asked for it with
a HEAD chain gated hop by hop before anything is fetched, so the announcement and the parts carry
the number first, and the row keeps it. A host that does not say leaves the number out rather
than guessing it.

Every file goes through the download tracker (`GET /models/download/progress`), whose entries
carry `model_id` and `part` (`model` or `pictures`). One transfer runs per tracker key: a second
request for a file in flight starts nothing and answers `already_downloading`, and a resume of
a running transfer does the same. A running transfer holds a clone of its entry's control flag,
which is how an entry left at `downloading` by a task that died is told from a live one. A
failed entry's `error` says what happened and what to do in plain words
(`crates/pond-api/src/download_failure.rs`); the raw error goes to the log. `POST /models/download/control` takes
`{filename, action}` for one file or `{model_id, action}` for every part of a model, with
`action` one of `pause`, `resume` or `cancel`. Pause keeps the partial file, cancel deletes it,
cancelling a model's own file cancels its add-on too, and a paused entry is never evicted. A
row's `companions` read `downloading` while the add-on comes down, `verifying` while its hash is
checked, then `installed`. When the model file arrives its row is marked downloaded,
and when either part arrives the agent registers the model by the file its row names and verifies
and attaches the add-on (`Agent::prepare_model`, which never downloads). Nothing else starts a
fetch: not choosing a model, not saving settings, not a provider build, not a refused picture
turn. The boot restore (`restore_assigned_models`) fetches only a model whose file is missing,
of which no other quant is on disk, and which is assigned to a role that loads a model (chat,
tool, asr, tts or embedding; never think or task, which select nothing yet), and its add-on only
if it had one, through the same tracker.

## Storage layout

`crates/pond-core/src/models/domain/model_layout.rs` is the one mapping from (category, file) to a
path. It keeps only a name's last component. Storage, the routes, the CLI and disk usage all use
it.

## Seeding and pruning

Every seed upserts the catalogue and prunes what it leaves stale
(`model_service::apply_catalog`): companion rows, custom rows that lost their file and have no
source, bundled rows no longer bundled and never downloaded, and Ollama rows a running Ollama
server no longer lists. Anything assigned stays, nothing the fetch itself lists is pruned, a row
whose file is on disk stays unless it is a companion (the downloaded flag can lag the disk), and
no file is touched. An Ollama that does not answer, or answers with no models, changes none of
its rows: the fetch cannot tell the two apart, so the last model removed from Ollama leaves its
row behind. Rows are told apart by exact id; `Foo` and `foo` are two rows.

Deleting a model refuses when another row that is assigned names the same file (an older row
under another id, or another spelling of the name).

## No model chosen

With no conversation model, the chat routes answer `409 {"code": "no_model"}` before anything is
saved, the voice child says so as an NDJSON `error` event, and warm-up and quiet compaction skip.
Nothing is picked or downloaded in its place.
