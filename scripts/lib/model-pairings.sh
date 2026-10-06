#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
# model-pairings.sh — the vision pairing table: which encoder (mmproj) each known model needs.
#
# Sourced, never run: it defines functions and does nothing else. Used by scripts/giap.sh
# (`giap.sh models pairings`, and doctor).
#
# The table is crates/pond-core/data/vision-pairings.jsonl, one JSON object per line, which pond-core
# embeds. scripts/models/vision_pairings.py regenerates it from Hugging Face (Python 3.9+, standard
# library only), and .github/workflows/model-pairings.yml does that weekly and proposes the change as
# a pull request. The pond never fetches the table itself.
#
# Two kinds of function. The data functions (paths, pairings_state) print nothing beyond the value
# asked for and work under `set -euo pipefail`. pairings_generate and pairings_report talk to a person
# through the caller's ok/warn/bad/info/note/run, as giap.sh and the tests define them.
#
# Written for bash 3.2, the only bash on a stock macOS: no `declare -A`, no `mapfile`, no `${x,,}`.
# ─────────────────────────────────────────────────────────────────────────────

MODEL_PAIRINGS_REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"

# ── where things live ────────────────────────────────────────────────────────

# Overridable so a test never touches the committed table.
pairings_file()      { printf '%s' "${GIAP_PAIRINGS_FILE:-$MODEL_PAIRINGS_REPO/crates/pond-core/data/vision-pairings.jsonl}"; }
pairings_generator() { printf '%s' "$MODEL_PAIRINGS_REPO/scripts/models/vision_pairings.py"; }
# The table's path as a person reads it: relative to the repo when it is inside it.
pairings_file_shown() {
  local f; f="$(pairings_file)"
  case "$f" in "$MODEL_PAIRINGS_REPO"/*) printf '%s' "${f#"$MODEL_PAIRINGS_REPO"/}" ;; *) printf '%s' "$f" ;; esac
}

# The interpreter for the generator: GIAP_PYTHON when set, else python3. Fails when there is none.
pairings_python() {
  local p
  p="$(command -v "${GIAP_PYTHON:-python3}" 2>/dev/null)" || return 1
  [ -n "$p" ] || return 1
  printf '%s' "$p"
}

# ── state, for doctor ────────────────────────────────────────────────────────

# pairings_state: PAIRINGS_STATE is missing, empty or present; PAIRINGS_LINES counts the lines;
# PAIRINGS_NEWEST is the newest checked_at, empty when no line has one. Reads the file, parses nothing.
pairings_state() {
  local f; f="$(pairings_file)"
  PAIRINGS_STATE=missing; PAIRINGS_LINES=0; PAIRINGS_NEWEST=""
  [ -f "$f" ] || return 0
  PAIRINGS_LINES="$(grep -c '[^[:space:]]' "$f" 2>/dev/null)" || PAIRINGS_LINES=0
  PAIRINGS_NEWEST="$(sed -n 's/.*"checked_at"[[:space:]]*:[[:space:]]*"\([0-9]\{4\}-[0-9][0-9]-[0-9][0-9]\)".*/\1/p' "$f" \
    | LC_ALL=C sort | tail -1)"
  if [ "$PAIRINGS_LINES" -gt 0 ]; then PAIRINGS_STATE=present; else PAIRINGS_STATE=empty; fi
  return 0
}

# ── talking (giap.sh supplies ok/warn/bad/info/note/run) ─────────────────────

# Show a command, then run it: the caller's `run` when there is one, which also honours --dry-run.
_pairings_run() {
  if declare -F run >/dev/null 2>&1; then run "$@"; return; fi
  printf '$ %s\n' "$*"
  "$@"
}

# One line for doctor. Information only: a missing or old table never fails a pond.
pairings_report() {
  pairings_state
  case "$PAIRINGS_STATE" in
    missing)
      info "no vision pairing table at $(pairings_file_shown)"
      note "make it: bash scripts/giap.sh models pairings" ;;
    empty)
      info "the vision pairing table at $(pairings_file_shown) is empty" ;;
    *)
      info "vision pairing table: $PAIRINGS_LINES pairings, newest checked_at ${PAIRINGS_NEWEST:-none}"
      note "refreshed weekly by a reviewed pull request; compare it now: bash scripts/giap.sh models pairings --check" ;;
  esac
  return 0
}

# pairings_generate [true]: regenerate the table, or with `true` only compare it, against Hugging Face.
# Returns the generator's status: 0 done or up to date, 1 stale (compare only), 2 bad input,
# 3 a request failed and nothing was written. Under DRY_RUN=true it prints the command and returns 0.
pairings_generate() {
  local py rc
  if ! py="$(pairings_python)"; then
    bad "no ${GIAP_PYTHON:-python3} here; the generator needs Python 3.9 or newer, standard library only"
    return 1
  fi
  if [ "${1:-false}" = true ]; then set -- --check; else set --; fi
  _pairings_run "$py" "$(pairings_generator)" --out "$(pairings_file)" "$@"; rc=$?
  [ "${DRY_RUN:-false}" = true ] && return 0
  case "$rc/$#" in
    0/1) ok "the committed table matches Hugging Face" ;;
    0/*) pairings_state
         ok "$(pairings_file_shown): $PAIRINGS_LINES pairings, newest checked_at ${PAIRINGS_NEWEST:-none}"
         note "review the diff and commit it; pins reach a pond only through a reviewed change" ;;
    1/*) warn "regenerating would change the table (the lines above say how)"
         note "update it: bash scripts/giap.sh models pairings" ;;
    3/*) bad "a request to Hugging Face failed; nothing was written" ;;
    *)   bad "the generator stopped (exit $rc); nothing was written" ;;
  esac
  return $rc
}
