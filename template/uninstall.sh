#!/system/bin/sh
# Executed by the module manager when this module is removed.
#
# The module directory is deleted by the manager, but the WebUI's caches live
# outside it so that they survive a module update. That means they have to be
# cleaned up here, or they would be left behind with no owner.
#
# Only WebUI-owned caches are removed. The keystore and injector data under
# /data/misc/keystore/omk and /data/adb/omk belongs to the running services and
# is deliberately left alone, since a reinstall is expected to pick it back up.

rm -rf /data/misc/keystore/omk/data/webui
rm -f /data/adb/omk/flashstation-cache.json
