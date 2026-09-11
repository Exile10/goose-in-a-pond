#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
# macos-sdk.sh — pin SDKROOT to the SDK that belongs to the active toolchain.
#
#   source "$(dirname "$0")/lib/macos-sdk.sh"
#
# A no-op on anything but macOS, and a no-op when SDKROOT is already set.
#
# ── Why this exists ──────────────────────────────────────────────────────────
#
# Installing the Command Line Tools separately from Xcode leaves two SDKs on the
# machine, and clang and ld can end up on opposite sides of it:
#
#   xcode-select -p        → /Applications/Xcode.app/...        (so ld is Xcode's)
#   xcrun --show-sdk-path  → .../Xcode.app/.../MacOSX26.5.sdk   (the matching SDK)
#   clang's default        → /Library/Developer/CommandLineTools/SDKs/MacOSX.sdk
#                            → MacOSX27.0.sdk                   (a DIFFERENT SDK)
#
# Xcode's linker then reads the newer SDK's stub library and does not recognise
# a slice that postdates it:
#
#   ld: tapi error: malformed file
#   .../MacOSX27.0.sdk/usr/lib/libSystem.B.tbd:4:20: error: unknown architecture
#                      arm64e.x1-macos, arm64e.x1-maccatalyst ]
#
# Every crate that compiles C hits it. In this workspace that is `aws-lc-sys`,
# which means pond-api, pond-infra, pond-mcp-server, pond-adapters-goose and
# pond-server all fail to build, while `pond-core` — which pulls none of that
# chain — builds fine.
#
# That asymmetry is the dangerous part, and it is why this is a build hook
# rather than a note in a README. A session that runs `cargo test -p pond-core`,
# sees 1,500 green tests and calls the tree healthy is reporting on the one
# crate the breakage cannot reach. That has already happened twice here: two
# review agents hit this failure, silently skipped the runs they could not
# perform, and reported pond-core's green as the result.
#
# ── The rule ─────────────────────────────────────────────────────────────────
#
# Use the SDK from the toolchain `xcode-select` selected, because that is the
# toolchain whose `ld` is about to run. Not "the newest SDK", not a hardcoded
# path — either of those rots at the next Xcode or CLT update. `xcrun` answers
# this by definition, so it is the source of truth and the version numbers above
# are illustration, not configuration.
#
# A pre-set SDKROOT is left alone: someone who set it meant it.
# ─────────────────────────────────────────────────────────────────────────────

giap_pin_macos_sdk() {
  [ "$(uname -s 2>/dev/null)" = "Darwin" ] || return 0

  if [ -n "${SDKROOT:-}" ]; then
    return 0
  fi

  command -v xcrun >/dev/null 2>&1 || return 0

  # The SDK belonging to the active toolchain. If xcrun cannot answer, the
  # install is broken in a way this shim should not paper over — say nothing
  # and let the real error surface.
  local want
  want="$(xcrun --sdk macosx --show-sdk-path 2>/dev/null)" || return 0
  [ -n "$want" ] && [ -d "$want" ] || return 0

  # What clang would pick on its own.
  local have
  have="$(clang -v -E -x c /dev/null 2>&1 \
            | grep -m1 -o -- '-isysroot [^ ]*' \
            | cut -d' ' -f2)" || true

  # Agreeing is the normal case and needs no intervention.
  [ -n "$have" ] && [ "$have" != "$want" ] || return 0

  export SDKROOT="$want"
  printf '  [sdk] clang defaulted to %s\n' "$have" >&2
  printf '  [sdk] pinned SDKROOT=%s (the active toolchain'"'"'s own SDK)\n' "$want" >&2
}

# Check without changing anything. Returns 1 when the two disagree, so a doctor
# can report it and a build can act on it from the same rule.
#
# Echoes "<clang-default>|<xcrun-answer>" on divergence so a caller can name
# both without re-deriving them.
giap_macos_sdk_diverges() {
  [ "$(uname -s 2>/dev/null)" = "Darwin" ] || return 0
  command -v xcrun >/dev/null 2>&1 || return 0

  local want have
  want="$(xcrun --sdk macosx --show-sdk-path 2>/dev/null)" || return 0
  have="$(clang -v -E -x c /dev/null 2>&1 \
            | grep -m1 -o -- '-isysroot [^ ]*' \
            | cut -d' ' -f2)" || return 0

  [ -n "$want" ] && [ -n "$have" ] || return 0
  [ "$have" = "$want" ] && return 0

  printf '%s|%s' "$have" "$want"
  return 1
}
