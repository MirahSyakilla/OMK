MODDIR=${0%/*}
TARGET_DIR=/data/misc/keystore/omk
LOG_DIR=$TARGET_DIR/logs
TARGET_KEYBOX=$TARGET_DIR/keybox.xml
TARGET_INJECTOR_CONFIG=$TARGET_DIR/injector.toml
STATE_DIR=/data/adb/omk


mkdir -p "$TARGET_DIR"
chmod 0770 "$TARGET_DIR"
chown 1017:1017 "$TARGET_DIR"

mkdir -p "$LOG_DIR"
chmod 0770 "$LOG_DIR"
chown 1017:1017 "$LOG_DIR"

mkdir -p "$STATE_DIR"
rm -f "$STATE_DIR/keymint-daemon.pid" "$STATE_DIR/injector-daemon.pid"
rm -f "$STATE_DIR/restart.keymint" "$STATE_DIR/restart.injector" "$STATE_DIR/restart.all"

if [ ! -f "$TARGET_KEYBOX" ] && [ -f "$MODDIR/keybox.xml" ]; then
  cp "$MODDIR/keybox.xml" "$TARGET_KEYBOX"
fi

if [ ! -f "$TARGET_INJECTOR_CONFIG" ] && [ -f "$MODDIR/injector.toml" ]; then
  cp "$MODDIR/injector.toml" "$TARGET_INJECTOR_CONFIG"
fi

if [ -f "$TARGET_KEYBOX" ]; then
  chmod 0600 "$TARGET_KEYBOX"
  chown 1017:1017 "$TARGET_KEYBOX"
fi

for slot_keybox in "$TARGET_DIR"/keybox-slot-*.xml; do
  [ -f "$slot_keybox" ] || continue
  chmod 0600 "$slot_keybox"
  chown 1017:1017 "$slot_keybox"
done

if [ -f "$TARGET_INJECTOR_CONFIG" ]; then
  chmod 0600 "$TARGET_INJECTOR_CONFIG"
  chown 1017:1017 "$TARGET_INJECTOR_CONFIG"
fi

brene_owns_security_patch() {
  local dir cfg
  for dir in /data/adb/modules/brene /data/adb/modules/BRENE; do
    [ -f "$dir/disable" ] && continue
    [ -f "$dir/remove" ] && continue
    cfg="$dir/config.sh"
    [ -f "$cfg" ] || continue
    if grep -q '^[[:space:]]*config_spoof_os_security_patch_level_property[[:space:]]*=[[:space:]]*["'\'']\{0,1\}1["'\'']\{0,1\}[[:space:]]*$' "$cfg"; then
      return 0
    fi
  done
  return 1
}

apply_security_patch() {
  if brene_owns_security_patch; then
    return 0
  fi

  local val=""
  local cfg="$TARGET_DIR/config.toml"
  local prop="$STATE_DIR/integrity.prop"
  local rp=""

  if [ -f "$cfg" ]; then
    val=$(sed -n 's/^security_patch[[:space:]]*=[[:space:]]*"\([^"]*\)".*/\1/p' "$cfg" | head -n 1)
  fi

  case "$val" in
    auto|"")
      return 0
      ;;
    latest)
      if [ -f "$prop" ]; then
        val=$(sed -n 's/^SECURITY_PATCH=//p' "$prop" | head -n 1)
      fi
      if [ -z "$val" ] || [ "$val" = "latest" ]; then
        val=$(date +%Y-%m-05)
      fi
      ;;
  esac

  case "$val" in
    [0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9]) ;;
    *) return 0 ;;
  esac

  if [ -x /data/adb/ksu/bin/resetprop ]; then
    rp=/data/adb/ksu/bin/resetprop
  elif [ -x /data/adb/magisk/resetprop ]; then
    rp=/data/adb/magisk/resetprop
  elif [ -x /system/bin/resetprop ]; then
    rp=/system/bin/resetprop
  elif [ -x /data/adb/ksud ]; then
    /data/adb/ksud resetprop -n ro.build.version.security_patch "$val" >/dev/null 2>&1
    return 0
  else
    return 0
  fi

  "$rp" -n ro.build.version.security_patch "$val" >/dev/null 2>&1
}

apply_security_patch
