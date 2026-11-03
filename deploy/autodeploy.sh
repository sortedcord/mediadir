#!/usr/bin/env bash
# Pull origin/main; if it moved, build, install, restart, health-check; roll back on failure.
set -euo pipefail

REPO=${REPO:-/root/mediadir}
BRANCH=${BRANCH:-main}
BIN=${BIN:-/usr/local/bin/mediadir-server}
SERVICE=${SERVICE:-mediadir-server}
HEALTH=${HEALTH:-http://127.0.0.1:8096/api/health}
STATE=${STATE:-/var/lib/mediadir-deploy}
export PATH="$HOME/.cargo/bin:$PATH"

exec 9>/run/mediadir-autodeploy.lock
flock -n 9 || { echo "another deploy is running"; exit 0; }
mkdir -p "$STATE"

cd "$REPO"
git fetch -q origin "$BRANCH"
target=$(git rev-parse "origin/$BRANCH")
deployed=$(cat "$STATE/deployed" 2>/dev/null || true)
failed=$(cat "$STATE/failed" 2>/dev/null || true)
[[ "$target" == "$deployed" ]] && exit 0
[[ "$target" == "$failed" ]] && { echo "skipping $target (failed before)"; exit 0; }

echo "deploying ${deployed:0:7} -> ${target:0:7}"
git checkout -q --detach "$target"

fail() { echo "deploy of ${target:0:7} FAILED: $*"; echo "$target" > "$STATE/failed"; exit 1; }

( cd server && cargo build --release -q ) || fail "build"

cp -f "$BIN" "$STATE/previous" 2>/dev/null || true
install -m755 server/target/release/mediadir-server "$BIN.new" && mv -f "$BIN.new" "$BIN"
systemctl restart "$SERVICE"

for _ in $(seq 1 20); do
  if curl -fsS -m 2 "$HEALTH" >/dev/null 2>&1; then
    echo "$target" > "$STATE/deployed"; rm -f "$STATE/failed"
    echo "deployed ${target:0:7}"; exit 0
  fi
  sleep 1
done

echo "health check failed, rolling back"
if [[ -f "$STATE/previous" ]]; then
  install -m755 "$STATE/previous" "$BIN" && systemctl restart "$SERVICE"
fi
[[ -n "$deployed" ]] && git checkout -q --detach "$deployed"
fail "health check"
