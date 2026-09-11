#!/bin/sh
# Drop to PUID:PGID the way the *Arr containers do, so /config stays readable
# by the same user that owns everything else in appdata.
set -e

PUID=${PUID:-99}
PGID=${PGID:-100}
UMASK=${UMASK:-002}
CONFIG_PATH=${REPSETARR_CONFIG:-/config/config.yml}
CONFIG_DIR=$(dirname "$CONFIG_PATH")

umask "$UMASK"
mkdir -p "$CONFIG_DIR"

if [ ! -f "$CONFIG_PATH" ]; then
    cp /usr/local/share/repsetarr/config.example.yml "$CONFIG_PATH"
    echo "[repsetarr] no config found, wrote a starter one to $CONFIG_PATH"
fi

if [ "$(id -u)" != "0" ]; then
    echo "[repsetarr] running as $(id -u):$(id -g), leaving ownership alone"
    exec "$@"
fi

if ! getent group "$PGID" > /dev/null 2>&1; then
    groupadd -o -g "$PGID" repsetarr
fi
if ! getent passwd "$PUID" > /dev/null 2>&1; then
    useradd -o -u "$PUID" -g "$PGID" -M -d "$CONFIG_DIR" -s /usr/sbin/nologin repsetarr
fi

chown -R "$PUID:$PGID" "$CONFIG_DIR" 2>/dev/null \
    || echo "[repsetarr] could not chown $CONFIG_DIR, continuing"

echo "[repsetarr] starting as $PUID:$PGID (umask $UMASK, TZ ${TZ:-Etc/UTC})"
exec gosu "$PUID:$PGID" "$@"
