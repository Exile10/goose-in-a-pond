#!/usr/bin/env bash
# jetson-remote.sh -- reach the Jetson at one verified address for a whole deploy, and run its long
# build so that losing the connection cannot stop it. Sourced by scripts/jetson/deploy.sh; tested by
# jetson-remote.test.sh. Bash 3.2 compatible (macOS).
#
# The Mac reaches the device through an ssh alias (`nano`) whose HostName is a `.local` name, and
# mDNS on the home network drops out now and then. On 2026-10-12 it stopped resolving in the middle
# of a 50-minute build: the session running the build died, and the restart that should have
# followed could not find the device, which stayed down. Later the same name resolved only to an
# IPv6 link-local address. So:
#   - The address is decided once, at the start. While the name works, the device is asked for
#     its own IPv4 address; when it does not, the address that worked last time is used. Every
#     later call in the run goes to that one address.
#   - The device must present the NAME's host key (HostKeyAlias) at that address, so another
#     machine that has since taken the address is refused, not trusted.
#   - A call whose connection is lost is retried; a remote command that fails is not.
#   - The build runs detached on the device and this side follows its log, reconnecting as
#     needed, until it records an exit code. A deploy started while a build is still running
#     follows that build instead of starting a second one.

JETSON_ALIAS=""   # the ssh alias the device is known by (`nano`)
JETSON_NAME=""    # its configured HostName, whose host key the device must present
JETSON_ADDR=""    # the address this run uses
# Seconds before the first retry (doubling), and between log polls. Tests set them to 0.
: "${JETSON_RETRY_DELAY:=5}"
: "${JETSON_POLL_SECONDS:=10}"
# How long to keep trying to reach a device that is building, before giving up.
: "${JETSON_FOLLOW_PATIENCE:=3600}"
JETSON_REMOTE_DIR=".cache/giap-deploy"

jetson_cache_file() { printf '%s/jetson/%s.address\n' "${GIAP_STATE_DIR:-$HOME/.giap}" "$1"; }

jetson_is_ipv4() {
  case "$1" in '' | *[!0-9.]*) return 1 ;; esac
  local IFS=. octet
  # shellcheck disable=SC2086 # split on dots, deliberately
  set -- $1
  [ "$#" -eq 4 ] || return 1
  for octet in "$@"; do
    [ -n "$octet" ] && [ "$octet" -le 255 ] || return 1
  done
}

# The common options: the alias's user and key, a bounded connect, no prompts, and keepalives so a
# dead link is noticed in a minute rather than never.
jetson_ssh_opts() {
  printf '%s\n' -o ConnectTimeout=10 -o BatchMode=yes -o ServerAliveInterval=15 -o ServerAliveCountMax=4
}

# One call to the pinned address, checking the name's host key.
jetson_ssh_once() {
  # shellcheck disable=SC2046 # the options are one per line, none with spaces
  ssh $(jetson_ssh_opts) -o HostName="$JETSON_ADDR" -o HostKeyAlias="$JETSON_NAME" "$JETSON_ALIAS" "$@"
}

# As jetson_ssh_once, retrying a lost connection (ssh's own exit 255) with doubling waits. A remote
# command's own failure is returned at once: it is the caller's to handle.
jetson_ssh() {
  local try=1 rc delay="$JETSON_RETRY_DELAY"
  while :; do
    if jetson_ssh_once "$@"; then return 0; else rc=$?; fi
    [ "$rc" -ne 255 ] && return "$rc"
    [ "$try" -ge 5 ] && break
    echo "    (lost the connection to ${JETSON_NAME} at ${JETSON_ADDR}; trying again in ${delay}s)" >&2
    sleep "$delay"
    try=$((try + 1))
    delay=$((delay * 2))
  done
  echo "ERROR: ${JETSON_NAME} at ${JETSON_ADDR} stopped answering" >&2
  return 255
}

# rsync <local dir/> to <path on the device>, mirroring it, retried like jetson_ssh. rsync's 255,
# 12 and 30 are a lost or stalled connection.
jetson_rsync() {
  local src="$1" dest="$2" try=1 rc delay="$JETSON_RETRY_DELAY" shell
  shell="ssh $(jetson_ssh_opts | tr '\n' ' ')-o HostName=$JETSON_ADDR -o HostKeyAlias=$JETSON_NAME"
  while :; do
    if rsync -az --delete -e "$shell" "$src" "${JETSON_ALIAS}:${dest}"; then return 0; else rc=$?; fi
    case "$rc" in 255 | 12 | 30) ;; *) return "$rc" ;; esac
    [ "$try" -ge 5 ] && return "$rc"
    echo "    (copy to ${JETSON_NAME} interrupted; trying again in ${delay}s)" >&2
    sleep "$delay"
    try=$((try + 1))
    delay=$((delay * 2))
  done
}

# Decide the address for this run. Prints how; returns non-zero, with the reason on stderr, when the
# device cannot be reached at all.
jetson_pin() {
  JETSON_ALIAS="$1"
  # Every substitution here may fail by design, and deploy.sh runs under `set -e`: hence `|| ...`.
  JETSON_NAME="$(ssh -G "$JETSON_ALIAS" 2>/dev/null | awk '$1 == "hostname" { print $2; exit }')" || JETSON_NAME=""
  [ -n "$JETSON_NAME" ] || JETSON_NAME="$JETSON_ALIAS"
  local cache found=""
  cache="$(jetson_cache_file "$JETSON_ALIAS")"

  # While the name works, the device says which IPv4 address it uses: the source of its default
  # route. The address it was reached at may be IPv6 link-local, which is no use from here.
  # shellcheck disable=SC2046,SC2016 # options one per line; $3 etc. are the device's
  found="$(ssh $(jetson_ssh_opts) "$JETSON_ALIAS" \
    'ip -4 route get 1.1.1.1 2>/dev/null | awk "{ for (i = 1; i < NF; i++) if (\$i == \"src\") { print \$(i + 1); exit } }"' \
    2>/dev/null | tail -n 1)" || found=""
  if jetson_is_ipv4 "$found"; then
    JETSON_ADDR="$found"
    if jetson_ssh_once true 2>/dev/null; then
      mkdir -p "$(dirname "$cache")" && printf '%s\n' "$found" >"$cache"
      echo "    ${JETSON_ALIAS} (${JETSON_NAME}) is at ${JETSON_ADDR}; this deploy uses that address throughout"
      return 0
    fi
    # Reachable by name but not at its IPv4 address from here: keep to the name, as before.
    JETSON_ADDR="$JETSON_NAME"
    echo "    ${JETSON_NAME} answers by name but not at ${found} from here; using the name"
    return 0
  fi

  found="$(cat "$cache" 2>/dev/null)" || found=""
  if ! jetson_is_ipv4 "$found"; then
    echo "ERROR: ${JETSON_NAME} (ssh alias ${JETSON_ALIAS}) cannot be reached, and no earlier deploy recorded its address." >&2
    echo "  Check the device is on and on this network. Its address is on the router's client list." >&2
    return 1
  fi
  JETSON_ADDR="$found"
  if ! jetson_ssh_once true 2>/dev/null; then
    echo "ERROR: ${JETSON_NAME} cannot be reached by name, and ${found}, its address last time, does not answer as the same device." >&2
    return 1
  fi
  echo "    ${JETSON_NAME} cannot be reached by name right now; using ${JETSON_ADDR}, where it answered last time with the same host key"
}

# Run `command` on the device detached, under a lock, and follow it here. Returns its exit code.
# If a command with the same label is already running there (a deploy that lost its connection),
# follow that one instead: never two builds at once, and never a script rewritten under a running
# bash.
jetson_run_detached() {
  local label="$1" command="$2" dir="$JETSON_REMOTE_DIR" staged state
  state="$(jetson_ssh "mkdir -p ~/$dir && cd ~/$dir && if flock -n $label.lock true; then echo idle; else echo running; fi")" || return
  if [ "$state" = running ]; then
    echo "    a ${label} started by an earlier deploy is still running on the device; following it"
  else
    staged="$(mktemp -d "${TMPDIR:-/tmp}/giap-${label}.XXXXXX")"
    printf '%s\n' "$command" >"$staged/$label.cmd"
    cat >"$staged/run-$label.sh" <<EOF
#!/usr/bin/env bash
# Holds the lock for the whole run, so a second deploy sees it running; the exit code is written last.
cd "\$HOME/$dir" || exit 1
exec 9>"$label.lock"
flock -n 9 || exit 75
rm -f "$label.exit"
bash "$label.cmd" >"$label.log" 2>&1
echo \$? >"$label.exit"
EOF
    jetson_rsync_files "$staged" "$dir" || return
    rm -rf "$staged"
    jetson_ssh "cd ~/$dir && rm -f $label.exit $label.log && setsid -f bash run-$label.sh </dev/null >/dev/null 2>&1" || return
  fi
  jetson_follow "$label"
}

# Copy the files in a local directory into a directory on the device, leaving its other files.
jetson_rsync_files() {
  local src="$1" dest="$2" try=1 rc delay="$JETSON_RETRY_DELAY" shell
  shell="ssh $(jetson_ssh_opts | tr '\n' ' ')-o HostName=$JETSON_ADDR -o HostKeyAlias=$JETSON_NAME"
  while :; do
    if rsync -a -e "$shell" "$src/" "${JETSON_ALIAS}:${dest}/"; then return 0; else rc=$?; fi
    case "$rc" in 255 | 12 | 30) ;; *) return "$rc" ;; esac
    [ "$try" -ge 5 ] && return "$rc"
    sleep "$delay"
    try=$((try + 1))
    delay=$((delay * 2))
  done
}

# Print a detached command's log as it grows and return its exit code once recorded. A poll that
# cannot reach the device is retried until JETSON_FOLLOW_PATIENCE runs out: the build goes on there.
jetson_follow() {
  local label="$1" dir="$JETSON_REMOTE_DIR" shown=0 out line lines code waited=0
  while :; do
    # shellcheck disable=SC2016 # expanded on the device
    if out="$(jetson_ssh_once "cd ~/$dir 2>/dev/null || exit 3
      n=\$(wc -l <$label.log 2>/dev/null || echo 0)
      tail -n +$((shown + 1)) $label.log 2>/dev/null | head -n \$((n - $shown))
      echo \"@@lines \$n\"
      if [ -f $label.exit ]; then echo \"@@exit \$(cat $label.exit)\"; fi" 2>/dev/null)"; then
      waited=0
      lines=""
      code=""
      while IFS= read -r line; do
        case "$line" in
          "@@lines "*) lines="${line#@@lines }" ;;
          "@@exit "*) code="${line#@@exit }" ;;
          *) printf '%s\n' "$line" ;;
        esac
      done <<<"$out"
      [ -n "$lines" ] && shown="$lines"
      if [ -n "$code" ]; then return "$code"; fi
    else
      waited=$((waited + JETSON_POLL_SECONDS + 1))
      if [ "$waited" -ge "$JETSON_FOLLOW_PATIENCE" ]; then
        echo "ERROR: lost ${JETSON_NAME} for ${waited}s while its ${label} ran; it may still finish there." >&2
        echo "  Run the deploy again: it follows the running ${label} instead of starting another." >&2
        return 255
      fi
      echo "    (cannot reach ${JETSON_NAME} at ${JETSON_ADDR}; the ${label} carries on there, still trying)" >&2
    fi
    sleep "$JETSON_POLL_SECONDS"
  done
}
