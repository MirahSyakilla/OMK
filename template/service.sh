MODDIR=${0%/*}
STATE_DIR=/data/adb/omk

mkdir -p "$STATE_DIR"
mkdir -p "$STATE_DIR/logs"

daemon_alive() {
  script=$1
  for pid in $(pgrep -f "$script" 2>/dev/null); do
    [ "$pid" = "$$" ] && continue
    cmdline=$(tr '\0' ' ' < "/proc/$pid/cmdline" 2>/dev/null)
    case "$cmdline" in
      *"$script"*) return 0 ;;
    esac
  done
  return 1
}

start_daemon() {
  script=$1
  pidfile=$2

  lock="$STATE_DIR/.$(basename "$script").lock"
  if ! mkdir "$lock" 2>/dev/null; then
    lock_pid=$(cat "$lock/pid" 2>/dev/null)
    if [ -n "$lock_pid" ] && kill -0 "$lock_pid" 2>/dev/null; then
      return 0
    fi
    rm -rf "$lock" 2>/dev/null || return 1
    mkdir "$lock" 2>/dev/null || return 0
  fi
  echo $$ > "$lock/pid"
  trap 'rmdir "$lock" 2>/dev/null' EXIT INT TERM
  if daemon_alive "$script"; then
    rm -rf "$lock" 2>/dev/null
    return 0
  fi
  rm -f "$pidfile"

  logfile="$STATE_DIR/logs/$(basename "$script").log"
  if [ -f "$logfile" ]; then
    log_size=$(wc -c < "$logfile" 2>/dev/null)
    if [ -n "$log_size" ] && [ "$log_size" -gt 262144 ]; then
      mv "$logfile" "$logfile.1" 2>/dev/null || true
    fi
  fi
  setsid nohup sh "$script" >>"$logfile" 2>&1 &
  sleep 1
  if ! daemon_alive "$script"; then
    rm -f "$pidfile"
    rm -rf "$lock" 2>/dev/null
    return 1
  fi
  rm -rf "$lock" 2>/dev/null
  return 0
}

start_daemon "$MODDIR/daemon" "$STATE_DIR/keymint-daemon.pid"
start_daemon "$MODDIR/daemon-injector" "$STATE_DIR/injector-daemon.pid"

# Reflect the three runtime services in the manager's module card.
#
# The manager reads module.prop once when it lists modules, so the description
# is rewritten in place rather than shipped static. Integrity reports the
# configured state, not just a process: the zygisk companion is loaded into the
# GMS processes by ReZygisk and has no pid of its own to poll.
INTEGRITY_TOML=""
for candidate in /data/adb/omk/integrity.toml /data/misc/keystore/omk/data/integrity.toml; do
  if [ -f "$candidate" ]; then
    INTEGRITY_TOML="$candidate"
    break
  fi
done

INTEGRITY_ENABLED="false"
if [ -n "$INTEGRITY_TOML" ]; then
  # Toybox sed has no branch alternation, so `enabled = true` is matched on its
  # own and anything else, including a missing key, stays off.
  if sed -n 's/^enabled[[:space:]]*=[[:space:]]*true.*/true/p' "$INTEGRITY_TOML" | head -n 1 | grep -q true; then
    INTEGRITY_ENABLED="true"
  fi
else
  # No integrity.toml at all. That is the pre-feature state, and reporting
  # Integrity as live would claim a service that has never been configured.
  INTEGRITY_ENABLED="unconfigured"
fi

if daemon_alive "$MODDIR/daemon"; then DAEMON_STATE="✅"; else DAEMON_STATE="❌"; fi
if daemon_alive "$MODDIR/daemon-injector"; then INJECTOR_STATE="✅"; else INJECTOR_STATE="❌"; fi
case "$INTEGRITY_ENABLED" in
  true) INTEGRITY_STATE="✅" ;;
  unconfigured) INTEGRITY_STATE="🚫" ;;
  *) INTEGRITY_STATE="❌" ;;
esac

# The status line goes on its own line, and the sed replacement is written with
# a literal newline. Toybox sed has no `\n` escape in the replacement text and
# would write it out as the two characters, and the `|` in the status is the
# alternation delimiter, so it has to be escaped. A temp file also keeps the
# write atomic, which matters because the manager may read module.prop at any
# moment.
STATUS="Daemon: ${DAEMON_STATE} | Injector: ${INJECTOR_STATE} | Integrity: ${INTEGRITY_STATE}"
STATUS_PROP="$MODDIR/.module.prop.status.$$"
if printf 'description=%s\nForked OhMyKeymint with customizations\n' "$STATUS" > "$STATUS_PROP" 2>/dev/null; then
  # Keep every key except description, then append the new one, so updateJson
  # and the version fields survive a rewrite.
  if grep -v '^description=' "$MODDIR/module.prop" > "$STATUS_PROP.keys" 2>/dev/null; then
    if cat "$STATUS_PROP.keys" "$STATUS_PROP" > "$MODDIR/module.prop.new" 2>/dev/null; then
      mv "$MODDIR/module.prop.new" "$MODDIR/module.prop" 2>/dev/null
    fi
  fi
fi
rm -f "$STATUS_PROP" "$STATUS_PROP.keys" "$MODDIR/module.prop.new" 2>/dev/null
