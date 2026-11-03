# Auto-deploy

Pull-based continuous deployment for a single host (e.g. an LXC container). A systemd timer runs
`autodeploy.sh` every 2 minutes. When `origin/main` has a new commit it:

1. checks out the commit (detached) in `/root/mediadir`,
2. builds `server` with `cargo build --release`,
3. installs the binary to `/usr/local/bin/mediadir-server` (previous one kept in `/var/lib/mediadir-deploy/previous`),
4. restarts `mediadir-server` and polls `/api/health` for 20 s,
5. on failure rolls back to the previous binary and records the commit as failed so it isn't retried; the next new commit is tried normally.

No inbound access or webhook is needed. The repo must be cloneable without credentials (public, or a deploy key).

## Install

```sh
cp deploy/mediadir-autodeploy.{service,timer} /etc/systemd/system/
systemctl daemon-reload
systemctl enable --now mediadir-autodeploy.timer
```

## Operate

```sh
journalctl -u mediadir-autodeploy -f          # deploy log
systemctl start mediadir-autodeploy           # deploy now
cat /var/lib/mediadir-deploy/deployed         # deployed commit
rm /var/lib/mediadir-deploy/failed            # retry a failed commit
```

Settings (env in the service unit): `REPO`, `BRANCH`, `BIN`, `SERVICE`, `HEALTH`, `STATE`.
