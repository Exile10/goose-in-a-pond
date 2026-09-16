#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
# macos-sdk.sh — make sure clang and ld come from the same toolchain.
#
#   source "$(dirname "$0")/lib/macos-sdk.sh"
#   giap_select_coherent_toolchain
#
# A no-op on anything but macOS, and a no-op when the toolchain is already
# coherent or the caller has pinned one.
#
# ── The failure ──────────────────────────────────────────────────────────────
#
# Installing the Command Line Tools separately from Xcode can leave the linker
# and the SDK on opposite sides of a version split:
#
#   ld: tapi error: malformed file
#   .../MacOSX27.0.sdk/usr/lib/libSystem.B.tbd:4:20: error: unknown architecture
#                      arm64e.x1-macos, arm64e.x1-maccatalyst ]
#
# Every crate that compiles C dies there. In this workspace that is `aws-lc-sys`,
# so pond-api, pond-infra, pond-mcp-server, pond-adapters-goose and pond-server
# all fail — while `pond-core`, which pulls none of that chain, builds green.
#
# That asymmetry is the reason this is a build hook and not a README note. The
# failure does not stop a session, it MISLEADS one: run pond-core, watch 1,500
# tests pass, call the tree healthy. Two review agents did exactly that — hit the
# link failure, skipped the runs they could not perform, and reported pond-core's
# green as the result.
#
# ── The rule ─────────────────────────────────────────────────────────────────
#
# A toolchain is COHERENT when clang's default SDK belongs to that same
# toolchain, because then its clang, its ld and its SDK are one install.
# Measured on the machine this was written for:
#
#   DEVELOPER_DIR=.../CommandLineTools   clang default -> CLT's SDK    COHERENT
#   DEVELOPER_DIR=.../Xcode.app/...      clang default -> CLT's SDK    mismatched
#
# clang defaults to the CLT SDK whichever toolchain is selected, so selecting
# Xcode is what produces the mismatch. Hence: SELECT a coherent toolchain rather
# than forcing an SDK onto an incoherent one. Both cure the link error, but
# pinning `SDKROOT=$(xcrun …)` pairs Xcode's older ld with Xcode's older SDK —
# it fights the machine, and it disagrees with what `~/.cargo/config.toml` does
# for bare `cargo` commands. One rule, applied in both places.
#
# Nothing here is hardcoded to a version: coherence is probed, not assumed, so
# the day Xcode catches up this stops firing on its own.
# ─────────────────────────────────────────────────────────────────────────────

# Candidates, most-preferred first. A coherent selection is kept as-is.
_giap_toolchain_candidates() {
  printf '%s\n' \
    "/Library/Developer/CommandLineTools" \
    "/Applications/Xcode.app/Contents/Developer"
}

# Is `dir` an internally consistent toolchain? True when clang, run under it,
# reaches for an SDK that lives inside it.
_giap_toolchain_is_coherent() {
  local dir="$1" sdk
  [ -x "$dir/usr/bin/clang" ] || [ -d "$dir" ] || return 1
  sdk="$(DEVELOPER_DIR="$dir" clang -v -E -x c /dev/null 2>&1 \
           | grep -m1 -o -- '-isysroot [^ ]*' | cut -d' ' -f2)" || return 1
  [ -n "$sdk" ] || return 1
  case "$sdk" in "$dir"*) return 0 ;; *) return 1 ;; esac
}

giap_select_coherent_toolchain() {
  [ "$(uname -s 2>/dev/null)" = "Darwin" ] || return 0
  command -v clang >/dev/null 2>&1 || return 0

  # Someone who pinned a toolchain meant it.
  [ -n "${DEVELOPER_DIR:-}" ] && return 0
  # So did someone who pinned an SDK; that is the other valid cure.
  [ -n "${SDKROOT:-}" ] && return 0

  local current
  current="$(xcode-select -p 2>/dev/null)" || return 0

  # Already fine: say nothing.
  if _giap_toolchain_is_coherent "$current"; then
    return 0
  fi

  local cand
  while IFS= read -r cand; do
    [ -d "$cand" ] || continue
    if _giap_toolchain_is_coherent "$cand"; then
      export DEVELOPER_DIR="$cand"
      printf '  [sdk] %s pairs its linker with another toolchain'"'"'s SDK\n' "$current" >&2
      printf '  [sdk] selected DEVELOPER_DIR=%s (clang, ld and SDK from one install)\n' "$cand" >&2
      return 0
    fi
  done < <(_giap_toolchain_candidates)

  # Nothing coherent. Forcing the SDK to match the active toolchain is the
  # weaker cure but still beats a link error.
  local want
  want="$(xcrun --sdk macosx --show-sdk-path 2>/dev/null)" || return 0
  [ -n "$want" ] && [ -d "$want" ] || return 0
  export SDKROOT="$want"
  printf '  [sdk] no coherent toolchain found; pinned SDKROOT=%s\n' "$want" >&2
}

# Report without changing anything. Returns 1 when the ACTIVE toolchain is
# incoherent, echoing "<active-dir>|<clang-default-sdk>" so a caller can name
# both without re-deriving them.
giap_macos_toolchain_incoherent() {
  [ "$(uname -s 2>/dev/null)" = "Darwin" ] || return 0
  command -v clang >/dev/null 2>&1 || return 0

  local current sdk
  current="$(xcode-select -p 2>/dev/null)" || return 0
  [ -n "$current" ] || return 0

  _giap_toolchain_is_coherent "$current" && return 0

  sdk="$(clang -v -E -x c /dev/null 2>&1 \
           | grep -m1 -o -- '-isysroot [^ ]*' | cut -d' ' -f2)"
  printf '%s|%s' "$current" "$sdk"
  return 1
}
