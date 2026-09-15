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
