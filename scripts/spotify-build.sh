#!/bin/sh
# YaJarvis + Spotify: build | deploy | rollback (see README, section "Сборка со Spotify")
set -eu
HERE="$(cd "$(dirname "$0")" && pwd)"
REPO_DIR="$(cd "$HERE/.." && pwd)"
VER="${JARVIS_VERSION:-0.10.16+spotify}"
HOST="${STATION_HOST:-}"
PORT="${STATION_PORT:-2222}"
KEY="${STATION_KEY:-}"
D=/data/opt/jarvisd
LOG=/tmp/jarvisd/jarvisd.log
SSH_OPTS="${KEY:+-i $KEY} -o StrictHostKeyChecking=accept-new -o ConnectTimeout=10"
rsh() { [ -n "$HOST" ] || { echo "set STATION_HOST to the speaker IP" >&2; exit 2; }; ssh -p "$PORT" $SSH_OPTS "root@$HOST" "$@"; }

# Number of lines currently in the speaker log (0 if missing).
log_lines() { rsh "wc -l < $LOG 2>/dev/null || echo 0" | tr -d ' \r'; }

# wait_started OFFSET GREP_REGEX LABEL
# Looks only at log lines written after OFFSET (reads from line 1 if the log shrank),
# waits up to 30 s for a match, then confirms jarvisd is still alive after 5 s.
wait_started() {
    off=$1; re=$2; label=$3; i=0
    while [ $i -lt 30 ]; do
        if rsh "t=\$(wc -l < $LOG); o=$off; [ \$t -lt \$o ] && o=0; tail -n +\$((o+1)) $LOG | grep -q '$re'" 2>/dev/null; then
            sleep 5
            if rsh "ps | grep -q '[j]arvisd -data'"; then
                echo "OK: $label build started and is still running"
                return 0
            fi
            echo "ERROR: $label build started but jarvisd is not running 5 s later" >&2
            rsh "tail -20 $LOG" >&2 || true
            exit 1
        fi
        sleep 1; i=$((i+1))
    done
    echo "ERROR: timeout waiting for $label start line in $LOG" >&2
    rsh "tail -20 $LOG" >&2 || true
    exit 1
}

case "${1:-}" in
build)
    export PATH="$HOME/.local/bin:$HOME/.cargo/bin:/opt/homebrew/opt/rustup/bin:$PATH"
    cd "$REPO_DIR"
    STATION_ARCHES=arm64 VERSION="$VER" JARVIS_VERSION="$VER" sh build.sh
    mkdir -p out
    # build.sh stages the arm64 core at build/station-build/arm64/jarvisd
    cp build/station-build/arm64/jarvisd out/jarvisd
    ls -l out/jarvisd
    ;;
deploy)
    [ -f "$REPO_DIR/out/jarvisd" ] || { echo "no out/jarvisd, run build first" >&2; exit 1; }
    rsh "[ -f $D/jarvisd.orig ] || cp $D/jarvisd $D/jarvisd.orig"
    # the speaker has no scp/sftp: stream over ssh
    rsh "cat > $D/jarvisd.new" < "$REPO_DIR/out/jarvisd"
    [ "$(rsh "wc -c < $D/jarvisd.new" | tr -d ' ')" = "$(wc -c < "$REPO_DIR/out/jarvisd" | tr -d ' ')" ] || { echo "upload size mismatch" >&2; exit 1; }
    N=$(log_lines)
    rsh "chmod +x $D/jarvisd.new && $D/jarvisd.sh stop && mv $D/jarvisd.new $D/jarvisd && $D/jarvisd.sh start"
    wait_started "$N" "jarvis $(printf '%s' "$VER" | sed 's/+/[+]/')" "$VER"
    ;;
rollback)
    rsh "[ -f $D/jarvisd.orig ]" || { echo "ERROR: $D/jarvisd.orig is missing on the speaker, cannot roll back" >&2; exit 1; }
    N=$(log_lines)
    rsh "cp $D/jarvisd.orig $D/jarvisd.new && chmod +x $D/jarvisd.new && $D/jarvisd.sh stop && mv $D/jarvisd.new $D/jarvisd && $D/jarvisd.sh start" \
        || { echo "ERROR: rollback failed" >&2; exit 1; }
    # stock version line has no "+suffix"
    wait_started "$N" 'jarvis [0-9][0-9.]* [(]' "stock"
    ;;
*) echo "usage: $0 build|deploy|rollback" >&2; exit 2 ;;
esac
