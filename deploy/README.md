# Deploying FF Multiverse (private)

**Status: nothing is deployed.** The target host, port, service account and network
exposure are all **UNKNOWN**: none of them has been assigned. This directory holds a
systemd user-unit template and the procedure to use once they are. Nothing in this
repository deploys anything or contacts any host; `scripts/package.sh` only writes a
tarball under `dist/`.

Wherever this document writes `<HOST>`, `<PORT>` or `<USER>`, the real value is not known
yet. Do not guess one and do not commit one: the repository must stay free of host names,
addresses and machine identifiers (`/deploy/*.env` is git-ignored for that reason).

## What gets deployed

One tarball from `scripts/package.sh`: `fmv-server`, `fmv-client`, `fmv-import`,
`README.md`, `LICENSE-NOTICE.txt` and `manifest.json`, and nothing else (an allowlist
guard enforces this). **It contains no game data.** The `FIXTURE` area needs none; any
real FF7 area needs the owner's own legally owned copy, imported on that host with
`fmv-import`. Owned data never travels in a release tarball and never crosses the wire
between server and clients.

## Values that are not assigned yet (all UNKNOWN)

| Item | Value | Notes |
|---|---|---|
| Target host | UNKNOWN | Not decided. |
| Service account (`<USER>`) | UNKNOWN | Must be an ordinary user: the unit is a user unit, no root. |
| Listen address and port (`FMV_BIND`) | UNKNOWN | Placeholder only; see "Configuration". |
| Network exposure | UNKNOWN | M1 targets a trusted private network (plan section 4); it has no hardened auth or encryption. Do not expose it publicly. |
| Transport for the tarball | UNKNOWN | Owner decides; none is scripted. |
| Base directory | `~/servers/ff-multiverse` | A proposal, used consistently below and in the unit. |
| Linger (start without a login session) | UNKNOWN | Needed for boot start; may need admin rights once. |
| State database path | UNKNOWN | Defined by `fmv-persist` when it lands. |
| Oldest glibc to support | UNKNOWN | Binaries link the build machine's glibc dynamically; check the target before the first deploy. |

## Install layout

```
~/servers/ff-multiverse/
  releases/
    ff-multiverse-<version>-<gitshort>-linux-x86_64/   one unpacked package per release
  current  -> releases/<package name>                  the only path the unit executes
  previous -> releases/<package name>                  last known-good release, for rollback
  config/ff-multiverse.env                             FMV_BIND=...   (chmod 600, never in git)
  state/                                               persistent data: DB, library
  backups/                                             snapshots taken before each upgrade
~/.config/systemd/user/ff-multiverse.service           copy of deploy/ff-multiverse.service
```

- **Versioned directories plus a `current` symlink.** A release is unpacked once into its
  own directory and never modified or overwritten. Switching versions means re-pointing
  `current`; that is also the whole rollback mechanism.
- **State is separate.** `state/` is never inside `releases/`, so replacing or deleting a
  release can never touch players' data. It is also the unit's `WorkingDirectory`. The
  unit sets `XDG_DATA_HOME` to it because the plan keeps writable state under
  `$XDG_DATA_HOME/ff-multiverse/`; that is the intended interface and still to be
  verified once `fmv-persist` defines the real setting.

## Configuration

`config/ff-multiverse.env` (create it by hand; mode 600; EnvironmentFile syntax, so no
inline comments):

```
# FMV_BIND is UNKNOWN until a host and port are assigned. Replace this placeholder; while
# it is not a socket address the server refuses to start (fail closed).
FMV_BIND=<HOST>:<PORT>
```

The unit starts `fmv-server` with its defaults for everything else. The ones that matter once
the port is reachable by more than one machine are the abuse limits (each has a flag; see
the "Limits" table in the top-level README and `fmv-server --help`): a connection must say
`Hello` within 3 s, one address holds at most 8 connections and may create 10 accounts per 10
minutes (players behind one NAT share that), the server creates at most 120 accounts per
minute and holds at most 100000, and a joined session that only answers pings is dropped after
30 minutes. To change one, add the flag to `ExecStart` (for example `--max-connections-per-ip 16`
for a LAN party behind one NAT). The protocol is still not encrypted: that does not change.

## First install

Run on the target host as `<USER>`, after the owner has assigned the UNKNOWN values.

1. Create the layout. The directories must exist before the first start, otherwise the
   unit fails (`WorkingDirectory` and `EnvironmentFile` are not optional):

   ```
   mkdir -p ~/servers/ff-multiverse/{releases,config,state,backups}
   ```

2. Get `<name>.tar.gz` and `<name>.manifest.json` from the build machine's `dist/` onto the
   host, together with `deploy/ff-multiverse.service` from the repository checkout: the
   unit template is deliberately not part of the release tarball. The transport is
   UNKNOWN and deliberately not scripted.
3. Verify the archive against the sidecar manifest (the two values must be identical):

   ```
   sha256sum <name>.tar.gz
   jq -r .tarball.sha256 <name>.manifest.json
   ```

4. Unpack into a new release directory and verify the binaries against the manifest inside
   the archive:

   ```
   tar -xzf <name>.tar.gz -C ~/servers/ff-multiverse/releases/
   cd ~/servers/ff-multiverse/releases/<name>
   jq -r '.binaries | to_entries[] | "\(.value.sha256)  \(.key)"' manifest.json | sha256sum -c -
   ```

   A name ending in `-dirty` was built from uncommitted changes and one ending in
   `-prebuilt` was not compiled by the packaging run. Neither is a clean release; do not
   deploy them.
5. Point `current` at it and write the env file (see "Configuration"):

   ```
   ln -sfn releases/<name> ~/servers/ff-multiverse/current
   ```

6. Install and start the unit (`<path to>` is wherever step 2 put the file):

   ```
   mkdir -p ~/.config/systemd/user
   cp <path to>/ff-multiverse.service ~/.config/systemd/user/
   systemctl --user daemon-reload
   systemctl --user enable --now ff-multiverse
   ```

7. Check that it came up:

   ```
   systemctl --user status ff-multiverse
   journalctl --user -u ff-multiverse -n 50
   ```

   The journal must show the server's `READY protocol=... addr=... area=... fixture=...`
   line. Then connect two headless bots (`fmv-client --server <HOST>:<PORT> --name A
   --headless --duration 5`, and the same with `--name B`) and confirm each prints a
   `SMOKE` line with `peers_seen>=1`.
8. For the server to run without an interactive login, user services need lingering,
   which is UNKNOWN on the target: `loginctl enable-linger <USER>`.

Before shipping any package, smoke-test the artifact itself on the build machine:
unpack it somewhere and run `scripts/smoke.sh --bin-dir <unpacked directory>`.

## Upgrade

1. Build a **clean** release (`scripts/package.sh` from a clean checkout) and smoke-test
   the unpacked artifact (above).
2. Transfer, verify and unpack it into a **new** `releases/<name>` (steps 2 to 4 above).
   Never unpack over an existing release.
3. **Snapshot the state before switching.** Stop the service first so nothing is mid-write,
   then take a cold copy, which is also safe for SQLite's `-wal` and `-shm` files:

   ```
   cd ~/servers/ff-multiverse
   systemctl --user stop ff-multiverse
   ts=$(date -u +%Y%m%dT%H%M%SZ)
   cp -a state "backups/$ts-before-<name>"
   ```

4. Remember the old release, then switch `current` atomically:

   ```
   cd ~/servers/ff-multiverse
   ln -sfn "$(readlink current)" previous
   ln -sfn releases/<name> current.new && mv -T current.new current
   ```

5. Start it and verify the journal shows the new `build=` in the READY line, then repeat
   the two-bot check:

   ```
   systemctl --user start ff-multiverse
   journalctl --user -u ff-multiverse -n 20
   ```

6. Keep the previous release and the snapshot until the new release has proven itself.
   Prune older `releases/` and `backups/` entries afterwards (keeping the last few).

## Rollback

1. Stop the service: `systemctl --user stop ff-multiverse`. Stopping sends `SIGTERM`; the
   unit allows `TimeoutStopSec=30` before `SIGKILL`.
2. Switch the symlink back to the release recorded in `previous`:

   ```
   cd ~/servers/ff-multiverse
   ln -sfn "$(readlink previous)" current.new && mv -T current.new current
   ```

3. **If the new release ran against the state, restore the snapshot taken before the
   upgrade.** A newer version may have migrated the database schema, and an older binary
   must not be pointed at state written by a newer schema. Move `state/` aside and copy
   the snapshot back (`<ts>` is the timestamp in the snapshot's directory name; see
   `ls backups/`):

   ```
   mv state state.failed-<ts>
   cp -a backups/<ts>-before-<name> state
   ```

   Anything players did after the snapshot is lost; that is the price of rolling back
   after a schema change, and the reason the snapshot comes before the switch.
4. Start the service and confirm the READY line shows the old `build=`.
5. Keep the failed release and `state.failed-*` until the cause is understood.

## Day-to-day

```
systemctl --user status|restart|stop ff-multiverse
journalctl --user -u ff-multiverse -f
```

`Restart=on-failure` restarts crashes after 3 s, at most 5 times in 2 minutes. Exit status
2 (a usage error such as a bad `FMV_BIND`) is never restarted.
