# FF Multiverse as an owner-only launcher port

This document is the contract between the FF Multiverse package and the game launcher that
installs and starts it. It describes what the package contains, how the launcher starts it, where
its data lives, what the first run does, and which device checks are still open.

Nothing here publishes anything. The package is built by `scripts/package.sh`; uploading it is the
launcher owner's existing publisher's job. This repository holds no hostnames, bucket names or
credentials, and neither does this document.

**Status:** the package and its entry point work from a plain unpack (checked end to end on the
build machine, see "Verification"). Not published. Controller, borderless full screen and the
on-screen keyboard are IMPLEMENTED but NOT RUN on a device (see "Device gate").

## 1. The package

One flat directory, no subdirectories, no symlinks, no game data (an allowlist guard checks this
three times while packaging):

| File | Role |
|---|---|
| `ff-multiverse.sh` | the launcher's `executable`: first-run import, local server, then the client |
| `fmv-client` | the launcher's `binary`: the game window (and the headless bot) |
| `fmv-server` | this machine's own local server, started by `ff-multiverse.sh` |
| `fmv-import` | reads the owner's own FF7 install into the library, started by `ff-multiverse.sh` |
| `README.md`, `LICENSE-NOTICE.txt` | documentation and the licence notice |
| `manifest.json` | name, version, `revision`, `build_id`, git commit, toolchain, sha256 of every file |

Not in the package, on purpose: any game file, and `banshee-install.json`. The launcher side writes
or requires that file itself (see section 7): its owner-package builder writes it into a hosted
package (a rollback to that version reads it), and its GitHub flow writes it at install time and
refuses a package that already carries one. Shipping none keeps both flows open.

**Naming.** `scripts/package.sh` writes `dist/ff-multiverse-<version>-<gitshort>[-dirty][-prebuilt]-linux-x86_64.tar.gz`.
That always matches the asset pattern `^ff-multiverse-.*linux-(x86_64|x64).*\.tar\.gz$`. A name
ending in `-dirty` came from uncommitted changes and one ending in `-prebuilt` from `--skip-build`:
neither is a release. The archive holds one top-level directory with the files above; an installer
that unwraps a single top-level directory gets them at the package root.

**Checksum asset.** `dist/SHA256SUMS` holds exactly one line, `<sha256>  <tarball file name>`, for the
package built by that run (it is replaced by every run: publish a package together with the
`SHA256SUMS` its own run wrote). `cd dist && sha256sum -c SHA256SUMS` checks it.

**Version label.** The launcher offers an update only when the installed and the available version
labels both contain an `r<N>` tag and the available `N` is larger (labels without it never count as
newer). `manifest.json` therefore carries `revision` (`git rev-list --count HEAD`) and `build_id`
(`ff-multiverse-r<revision>-<gitshort>`, plus `-dirty` or `-prebuilt` when they apply). Publish with
`build_id` as the version label, and only from builds of the integration branch: the revision grows
along one branch only.

**glibc.** The binaries link the build machine's glibc dynamically. Measured on the current build
machine: `fmv-client` needs glibc 2.43, `fmv-server` 2.35, `fmv-import` 2.34. The target machine's
glibc must be at least as new (`ldd --version`).

## 2. How the launcher starts it

The launcher runs `ff-multiverse.sh` directly (it is executable, `#!/usr/bin/env bash`). Its working
directory is the launcher's own per-game folder, not the package: the script finds its package from
its own location.

- **Arguments.** Everything is passed on to `fmv-client`, so `--headless --name Test --duration 5`
  runs a headless bot; without `--name` a window opens on the character menu. Two arguments are the
  script's own when they come first: `--stop-server` and `--wrapper-help`. `--help` and `--version`
  are answered by `fmv-client` without importing or starting anything. A first argument that is an
  absolute path is taken as the data directory and removed (a launcher that passes the data folder
  as an argument works; `fmv-client` has no positional argument, so nothing is lost).
- **Environment** (all optional):

  | Variable | Meaning |
  |---|---|
  | `FMV_DATA_DIR` | the data directory (below) |
  | `FMV_FF7_DIR` | FF7 folder to import from instead of searching Steam: the folder that holds `data/`, or the Steam Edition install root |
  | `FMV_FF7_LANGUAGE` | language to import, default `en` |
  | `FMV_FF7_REIMPORT=1` | import again although a library exists (for example after a build added battle data) |
  | `FMV_PORT` | the local server's port, default `47772` |
  | `FMV_READY_TIMEOUT`, `FMV_STOP_TIMEOUT` | seconds to wait for the server's READY line (60) and for it to exit after SIGTERM (30) |

- **Exit status.** After `exec` it is `fmv-client`'s own. Before that: `2` environment or usage error,
  `3` no FF7 copy found or its import failed (the client's own "content" code), `1` anything else
  (for example the local server did not start).
- **Messages.** A launcher shows a start that fails early only as "exited early, check the game's
  logs", so every message is also written to `<data dir>/launcher/launcher.log`.

## 3. The data directory

The script uses, first match wins: `$FMV_DATA_DIR`; a first argument that is an absolute path;
`${XDG_DATA_HOME:-~/.local/share}/ff-multiverse`.

How a launcher hands the folder over depends on the row it was given. For the launcher's generic
port provider (the one used for rows without a specialised provider):

- the data folder is `<managed library>/games/<id>/<dataFolder>`: outside the version folder, so it
  survives updates, and uninstall keeps `data` and `userdata`;
- `dataEnvironment` sets an environment variable of that name to the folder;
- `passDataDirectory` (default **true**) additionally puts the folder first in the argument list **and
  makes PLAY wait until that folder exists**, which an untouched first run never satisfies. The row
  below therefore sets `passDataDirectory` to **false** and uses the environment variable. The script
  copes with both.

Layout under the data directory:

| Path | Content |
|---|---|
| `library/` | the imported FF7 library (written by `fmv-import --state-dir <data dir>`) |
| `launcher/launcher.log` | what the script did and why it failed (one older generation kept) |
| `launcher/import.log` | the importer's output of the last first run |
| `launcher/server.log` | the server's output since its last start (the run before: `.1`) |
| `launcher/server.pid` | `<pid> <sha256 of the fmv-server it was started from>` |
| `launcher/server/` | the local server's accounts, avatars and character progress |
| `launcher/client/` | the client's saved logins, `accounts/<server address>.token` (mode 0600) |

Back up `launcher/server/` and `launcher/client/` together: a saved login is keyed by the server
**address**, so changing the port (`FMV_PORT`, or a future default) makes existing characters
unreachable until they are registered again, and their names stay taken. Keep `47772`.

Every binary the script starts also gets `FMV_STATE_DIR=<data dir>`, so nothing can fall back to a
state directory elsewhere.

## 4. First run: importing the owner's FF7

When `<data dir>/library` holds no FF7 set the script finds the owner's copy and runs
`fmv-import --state-dir <data dir> import-pc <folder> --language en`:

1. `FMV_FF7_DIR` if set (a missing folder is an error, not a search);
2. otherwise the Steam libraries of a native, Flatpak and Snap Steam
   (`~/.local/share/Steam`, `~/.steam/steam`, `~/.var/app/com.valvesoftware.Steam/...`,
   `~/snap/steam/common/...`, plus every folder listed in their `libraryfolders.vdf`, old and
   current format), looking for `steamapps/common/FINAL FANTASY VII Steam Edition` first and
   `steamapps/common/FINAL FANTASY VII` second. The importer identifies the edition itself.

The install is only ever read: the importer opens it read-only, checks it was not changed, and
refuses a state directory inside it; the script writes nothing outside the data directory. No
game byte is copied into the package, downloaded or sent to the server: the server and the client
each read their own library.

If FF7 is not found the script prints a clear message (what it searched, how to point it at the
game), writes it to `launcher.log` and exits `3`. If an install was found but cannot be imported it
prints the importer's error, names `import.log`, and exits `3`.

## 5. The local server

`ff-multiverse.sh` runs its own `fmv-server` on `127.0.0.1:47772` (loopback only; the protocol is not
encrypted) with `--game ff7 --ff7-library <data dir> --state-dir <data dir>/launcher/server
--debug-encounters`, waits for its READY line, and then becomes `fmv-client --game ff7 --server
127.0.0.1:47772 --ff7-library <data dir> --state-dir <data dir>/launcher/client`.

- **Reuse and restart.** A running server is reused when it is the pid recorded in `server.pid`,
  the packaged `fmv-server` has the recorded sha256, it was started with the same arguments, and the
  port answers. Otherwise only that recorded pid, after checking that it still is an `fmv-server` on
  this data directory's server state, gets SIGTERM (the server saves, then exits); SIGKILL only if
  it has not left after the timeout. Nothing else is ever signalled.
- **A foreign program on the port** is never joined: the script stops with a message (a character
  would be registered in someone else's server). Choose another port with `FMV_PORT`.
- **Detached.** The server is started in its own session and parented by init, with no inherited
  descriptors. A launcher's STOP kills the whole process tree of the process it started; the server
  is not part of it, so a progress save in flight is not cut by SIGKILL.
- **It keeps running** after the client closes, so the next start is instant and a package update
  replaces it. `ff-multiverse.sh --stop-server` ends it (use it before an uninstall).
  *Unverified:* whether a launcher shortcut that runs under Steam's Game Mode keeps showing as
  "running" while the detached server lives. Not tested on a device.
- **`--debug-encounters`** is on, so the B key starts a debug battle. It is a private owner build.

## 6. Proposed `ports.json` row

```json
{
  "id": "ff-multiverse",
  "appId": "banshee.ff-multiverse",
  "title": "FF Multiverse (FF7)",
  "source": "hosted",
  "assetRegex": "^ff-multiverse-.*linux-(x86_64|x64).*\\.tar\\.gz$",
  "checksumAsset": "SHA256SUMS",
  "gameRoot": "games/ff-multiverse",
  "executable": "ff-multiverse.sh",
  "binary": "fmv-client",
  "hostedFallback": false,
  "ownerOnly": true,
  "dataFolder": "data",
  "dataEnvironment": "FMV_DATA_DIR",
  "passDataDirectory": false
}
```

`source: hosted` means no GitHub repository (`repository` stays empty); `assetRegex` and
`checksumAsset` are only read for a GitHub source and are kept so the row is valid for either.

## 7. What the launcher side has to do

The package is only half of it. The launcher's owner has to:

1. add the row above and ship a launcher client release: for a hosted, owner-only row the client has
   no install path of its own yet, because the private-channel fetch is written for two fixed ids;
2. teach the owner builder and publisher about this game (both list their games by name), and
   publish with `build_id` as the version label;
3. allow the private-object key prefix `ff-multiverse/` in the account service that signs download
   URLs, and widen the scope of the read-only storage credential it uses to that prefix;
4. upload the tarball and `SHA256SUMS` with the existing publisher (never from this repository).

Item 3 is an access-policy and credential change and is the launcher owner's decision, not this
repository's. The concrete places are in the hand-off note that accompanies each package.

## 8. Device gate (owner testing after install)

The owner tests every private build on the handheld after installing it from the launcher. These
checks are post-install test items, **not** a shipment hold. All three are implemented in
`fmv-client`; none has been run on a device, a display or a controller (the build machine has none
of them). Each is recorded NOT RUN until the owner (or a later observed test) reports PASS or FAIL
with the device, OS, Game Mode / Gamescope and controller / Steam Input setup.

| Check | Implemented | Result |
|---|---|---|
| Controller works out of the box (gamepad through SDL / Steam Input, menus and gameplay) | **IMPLEMENTED** (NOT RUN: owner device test pending). gilrs, read as the same key events the keyboard makes; see "Controller" | NOT RUN |
| Window is borderless / full screen, no white border | **IMPLEMENTED** (NOT RUN: owner device test pending). `--fullscreen`, and by itself under gamescope; see "Full screen" | NOT RUN |
| Focusing a text field opens the on-screen keyboard | **IMPLEMENTED** (NOT RUN: owner device test pending). Steam's keyboard, gamescope sessions only; see "On-screen keyboard" | NOT RUN |

Per release, record next to this table: build id, source commit, package sha256, and for each check
its result. Never record PASS without an actual observation; virtual pads, unit tests and a headless
run support development but are not device results. Latest record: none (no build published).

### Controller

`fmv-client` reads game controllers with gilrs 0.11 (evdev and udev on Linux, hot-plug included) and
turns every button and stick into the key event the keyboard would have made, so there is no second
input path: the window feeds it to the same key dispatch as a key press, and the character menu
takes it as the same menu input. With no controller nothing changes.

| Controller | Key | In the field | In a battle | In the character menu |
|---|---|---|---|---|
| left stick, D-pad | arrows | walk (eight ways) | menu cursor, target pointer | move, change a value |
| A (south) | Enter | - | select / confirm | play, create, answer, dismiss a notice |
| B (east) | Backspace | - | back a level | back / cancel; never quits |
| X (west) | P | invite the nearest player | (same) | new character on the list, Backspace on the form |
| Y (north) | Y | accept the newest invite | (same) | forget a saved login (asks first) |
| Select / View | L | leave the party | (same) | - |
| Start / Menu | Esc | quit prompt: a second press quits | backs out of the menu, never quits | Esc: back, cancel, or quit |
| right bumper (R1/RB) | B | ask for a debug battle | - | - |
| left trigger (L2/LT), held | Shift | walk instead of run | - | - |

Nothing else is bound (the Guide/Steam button belongs to Steam). Details: a stick axis presses at 60 %
of its travel and lets go at 40 %; in menus and battles only the stronger axis counts and a held
direction repeats (after 0.4 s, then 8 times a second), in the field the stick reads in eight
directions and nothing repeats; no other button repeats. Only the window with the focus is
listened to (losing it lets go of everything). Two controllers at once act as one. The button names
come from an SDL mapping (gilrs's built-in database, plus `SDL_GAMECONTROLLERCONFIG`, which Steam
sets for the games it starts) or, failing that, from the driver's own codes. In that second case the
log line for the controller says "the driver's own layout" and X and Y may be swapped (the Linux xpad
driver is reported to send the Xbox X as the "north" code), which only exchanges invite and accept; a
controller with neither would do nothing and the log says so.

**Runtime dependency (new).** `fmv-client` is now linked to `libudev.so.1` (systemd-libs: present on
every systemd distribution, Bazzite included). It reads `/dev/input/event*`, which normally the seat
grants to the logged-in user. glibc floor: unchanged, still 2.43 on the current build machine.

**Not run, and known gaps.** Nothing here has seen a real controller. On a fresh install the create
screen asks for a name, which a controller alone can only type through Steam's keyboard: if the
automatic keyboard does not appear, Steam+X opens it by hand in Game Mode, so the controller check can
still be finished and the keyboard check reported on its own. The on-screen key hints (HUD and
menu) still name keyboard keys; the table above is the reference. gilrs does not honour
`SDL_GAMECONTROLLER_IGNORE_DEVICES`: if Steam Input hands over a virtual controller and the physical
one stays readable, both are heard and a key is simply held while either holds it, which is right
for the default layout but would mix with a custom Steam Input layout.

### Full screen

The window is a borderless full-screen window (winit `Fullscreen::Borderless` on the current monitor,
decorations off, so no title bar, no frame and no client-side frame drawn by winit under a compositor
that offers none) when `--fullscreen` is given or when ANY of these is true:

1. `GAMESCOPE_WAYLAND_DISPLAY` is set and not empty;
2. `XDG_CURRENT_DESKTOP`, a `:`-separated list, has an entry equal to `gamescope` (any case);
3. `SteamGamepadUI` is `1`.

Checks 1 and 2 are exported by gamescope itself (its `main.cpp`) to everything it starts, so a game
started from Game Mode inherits them, also through a launcher that Steam started (checked in
gamescope's source: both are set unconditionally before it starts the program, and
`WAYLAND_DISPLAY` is left unset unless gamescope runs with `--expose-wayland`, so under Game Mode the
client is normally an X11 client). Check 3 is Steam's, and is reported to be set for a game started
from Big Picture on an ordinary desktop too. `SteamDeck=1` is not a check: Steam sets it for games it
starts on Deck hardware, desktop mode is expected to be no exception (not verified), so it does not
mean Game Mode. A normal desktop session (KDE, started from a menu or a terminal) has none of the
three and stays windowed. The
start-up line `device: ...` on stderr names the signals seen and the decision. Not done: a
`--windowed` override and a run-time toggle.

About the "white border": the renderer cannot draw one. It clears to a dark blue-black everywhere,
the character menu paints a dark gradient over the whole window, and the bars around a field's 4:3
picture on a wider screen are that same dark colour (only a battle's own sky shows its horizon colour
in the part of the scene nothing covers). What full screen removes is the window frame and title bar,
which is the likely source; if the owner still sees a light border under gamescope, it is not this
window's frame and the report should say where it is (screen edge, around the picture, around the
menu).

### On-screen keyboard

Only inside a gamescope session (checks 1 and 2 above; **not** check 3 alone, so Big Picture on a
desktop gets full screen but no keyboard request) and only when a text field gains the focus (today:
the name field of the create screen, which a fresh install opens in), the client asks Steam for its
on-screen keyboard with `xdg-open steam://open/keyboard` (if that fails, `steam -ifrunning
steam://open/keyboard`), and with `steam://close/keyboard` when the field loses the focus or the form
is submitted. The request waits for the first frame on screen (or the first focus event), runs on a
helper thread with the environment unchanged and a 10 s limit, and is logged on stderr
(`keyboard: asked Steam to open its on-screen keyboard`, or why not). On a desktop nothing is ever
asked. While Steam's keyboard is up the controller drives it (B closes it), and its keys arrive as
ordinary key events with text, which the name field already takes: no input-method code is involved.
There is no password field (a masked one would need nothing different from this).

**Evidence, stated plainly.** Valve documents no such address for a program that is not a Steam
title (the documented route is the Steamworks call `ShowFloatingGamepadTextInput`, which needs a
Steam app id this owner-only port does not have). The addresses come from one third-party report of a
test on a Steam Deck in Game Mode, from a non-Steam shortcut, where both worked and the keys arrived as
key events (Laeresh/CSVM issue 111). We have not run them on a device. Open risks: whether
`xdg-open` reaches Steam from the launcher's environment on Bazzite; and a client that gamescope runs
as a Wayland client (`--expose-wayland`), where Steam may deliver text through the input-method
protocol instead, which this client does not use (it would then see no text).

### What to send back after a device test

The client's stderr, which for a start from the launcher is wherever the launcher keeps the game's
output. The lines that matter: `device: ...` (signals seen, window and keyboard decisions),
`window: using the X11 | Wayland windowing backend`, `gamepad: found | connected "<name>" (<usb
id>; <mapping>)`, `keyboard: ...`, and what the owner saw (borderless? a white border? did the
keyboard open? did every button in the table do what it says?).

Also not yet observed on a device: a start from a launcher that itself runs under Steam Game Mode.
The script and the binaries inherit the launcher's environment unchanged, so Steam's runtime
library path and overlay preload would reach `fmv-client`, and the detached local server may keep
the launcher's entry looking busy (see section 5). Both are NOT RUN.

## 9. Verification

Run on the build machine, from an unpacked `scripts/package.sh` tarball, with a temporary data
directory and the owner's own FF7 copy read-only:

- `bash -n scripts/ff-multiverse.sh` and, as a separate command, `bash -n scripts/package.sh`
  (`package.sh` also refuses to package an entry point that does not parse); shellcheck is not
  installed there: NOT RUN.
- `ff-multiverse.sh --headless --name Test --duration 5`: imports, prints the server's READY line,
  the bot prints its `SMOKE` line, exit 0. Second run reuses the server; a replaced `fmv-server`
  restarts it; a foreign listener on the port stops the start; `--stop-server` ends only the
  recorded server.
- no FF7 anywhere: clear message, exit 3. An empty `FINAL FANTASY VII Steam Edition` folder listed
  by a Flatpak Steam `libraryfolders.vdf`: found, import refused, exit 3.
- `scripts/package.sh`: archive lists `ff-multiverse.sh` (0755); `cd dist && sha256sum -c SHA256SUMS`.

Not run: any window mode (the build machine has no display), anything on the handheld, a real
launcher.
