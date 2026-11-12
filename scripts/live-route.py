#!/usr/bin/env python3
"""Play a written route through live fields (`fmv-server --field-runtime`), protocol 24: the acceptance bot.

A route file is an ordered list of entries, one per line: `FIELD: STEP; STEP; ...` (what
`runtime_explore --route-out` writes). The bot waits until it stands in FIELD and the field is idle (no script holds
the player, no window is open, no battle runs), then plays the steps:

    tri N        walk to the centre of walkmesh triangle N (planned over the walkmesh's links)
    xy X Y [Z]   walk to the point (X, Y) (planned the same way, then straight to the point; Z picks the level)
    to X Y       walk straight to (X, Y)
    gate N       walk across gateway slot N
    face DIR     face a field direction (0-255; 64 is +x)
    ok           press OK (the field's own scripts decide what it does)
    wait S       stand S seconds

An entry is over when its steps are done and the field is idle again, or when the server starts a new field visit.
Leaving an entry's field before its actions could start reports an obsolete route; those actions are not counted.
Windows are answered (first choice), movies acknowledged, battles left to the server's companions.
It prints what the server reports and ends with `ROUTE done=<entries played>/<entries> reached=<field> ...`
(exit status 0 only when every entry was played).

Walkmeshes come from the `walkmesh_json` example (game data: kept in --cache, outside the repository).
"""
import argparse
import importlib.util
import json
import math
import os
import subprocess
import sys
import time
from collections import deque
from pathlib import Path

HERE = Path(__file__).resolve().parent
spec = importlib.util.spec_from_file_location("livebot", HERE / "live-bot.py")
livebot = importlib.util.module_from_spec(spec)
spec.loader.exec_module(livebot)


def load_route(path):
    route = []
    for line in Path(path).read_text().splitlines():
        line = line.split("#", 1)[0].strip()
        if not line:
            continue
        field, steps = line.split(":", 1)
        route.append((field.strip(), [s.split() for s in steps.split(";") if s.strip()]))
    return route


class Mesh:
    def __init__(self, data):
        self.tris = [[tuple(v) for v in t["v"]] for t in data["triangles"]]
        self.links = [t["a"] for t in data["triangles"]]
        self.gates = data.get("gates", [])

    def centre(self, i):
        t = self.tris[i]
        return (sum(v[0] for v in t) / 3, sum(v[1] for v in t) / 3)

    def locate(self, x, y, z=None):
        best = None
        for i, t in enumerate(self.tris):
            (ax, ay, az), (bx, by, bz), (cx, cy, cz) = t
            d = (by - cy) * (ax - cx) + (cx - bx) * (ay - cy)
            if d == 0:
                continue
            l1 = ((by - cy) * (x - cx) + (cx - bx) * (y - cy)) / d
            l2 = ((cy - ay) * (x - cx) + (ax - cx) * (y - cy)) / d
            l3 = 1 - l1 - l2
            inside = min(l1, l2, l3)
            height = 0 if z is None else abs(l1 * az + l2 * bz + l3 * cz - z)
            key = (inside >= -0.02, -height if inside >= -0.02 else inside)
            if best is None or key > best[0]:
                best = (key, i)
        return best[1] if best else 0

    def height(self, i, x, y):
        (ax, ay, az), (bx, by, bz), (cx, cy, cz) = self.tris[i]
        d = (by - cy) * (ax - cx) + (cx - bx) * (ay - cy)
        if d == 0:
            return az
        l1 = ((by - cy) * (x - cx) + (cx - bx) * (y - cy)) / d
        l2 = ((cy - ay) * (x - cx) + (ax - cx) * (y - cy)) / d
        return l1 * az + l2 * bz + (1 - l1 - l2) * cz

    def path(self, start, goal):
        prev = {start: None}
        q = deque([start])
        while q:
            t = q.popleft()
            if t == goal:
                break
            for e, n in enumerate(self.links[t]):
                if n >= 0 and n not in prev:
                    prev[n] = (t, e)
                    q.append(n)
        if goal not in prev:
            return None
        points, t = [], goal
        while prev[t] is not None:
            p, e = prev[t]
            a, b = self.tris[p][e], self.tris[p][(e + 1) % 3]
            points.append(self.centre(t))
            points.append(((a[0] + b[0]) / 2, (a[1] + b[1]) / 2))
            t = p
        return list(reversed(points))


def mesh_for(field, cache, tool):
    f = Path(cache) / f"{field}.walk.json"
    if not f.exists():
        out = subprocess.run([tool, field], capture_output=True, text=True, timeout=60)
        f.write_text(out.stdout)
    return Mesh(json.loads(f.read_text()))


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("addr")
    ap.add_argument("route")
    ap.add_argument("--cache", required=True)
    ap.add_argument("--tool", required=True, help="the built walkmesh_json example")
    ap.add_argument("--seconds", type=float, default=900)
    ap.add_argument("--name", default="Router")
    ap.add_argument("--token-file", help="resume a throwaway test account; register and save its token when missing")
    ap.add_argument("--speed", type=float, default=200.0, help="walking speed, field units per second")
    ap.add_argument("--stall", type=float, default=60.0, help="give up after this long without progress")
    ap.add_argument("--verbose", action="store_true")
    args = ap.parse_args()
    route = load_route(args.route)
    token_path = Path(args.token_file) if args.token_file else None
    token = token_path.read_text().strip() if token_path and token_path.exists() else None
    if token_path and token_path.exists() and not token:
        ap.error("the token file is empty")
    auth = {"Token": token} if token else {"Register": {"display_name": args.name}}
    c = livebot.Conn(args.addr)
    c.send({"Hello": {"protocol_version": livebot.PROTOCOL, "client_build": "live-route", "auth": auth,
                      "character": {"tint": [80, 160, 220], "body": None, "slot": None}}})
    entity = area = mesh = None
    pos, facing, seq = None, 0.0, 0
    legs = []
    # Getting round whoever stands in the way: the corner being walked to, the nearest we came to it and when, how
    # many ways round were tried, and whether the first leg is such a way round.
    leg_goal, leg_best, leg_best_at, tries, tries_goal, detour, rejected = None, 0.0, 0.0, 0, None, None, 0
    locked = in_battle = False
    cast = []
    battle_at = 0.0
    waited_for = None
    windows = set()
    pending = []
    idx, steps = 0, None
    entry_expects_exit = False
    last_ping = last_move = busy_at = progress_at = time.monotonic()
    waiting_until = 0.0
    reached = []
    status = "time"
    end = time.monotonic() + args.seconds

    def plan(x, y, straight_end=True, z=None):
        start = mesh.locate(pos["x"], pos["y"], pos.get("z"))
        goal = mesh.locate(x, y, z)
        return (mesh.path(start, goal) or []) + ([(x, y)] if straight_end else [])

    while time.monotonic() < end:
        now = time.monotonic()
        if now - last_ping > 1:
            last_ping = now
            c.send({"Ping": {"nonce": int(now * 1000)}})
        for m in c.poll():
            k = livebot.kind(m)
            b = m[k] if isinstance(m, dict) else None
            if k == "Welcome":
                entity = b["entity"]
                if token_path and b.get("issued_token"):
                    with os.fdopen(os.open(token_path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600), "w") as f:
                        f.write(b["issued_token"])
            elif k in ("AreaManifest", "AreaChange"):
                previous_area = area
                area = b["area"]["key"]
                if k == "AreaChange":
                    # A scene can visit another field and return while control stays locked. Record the visit
                    # boundary now, before busy filtering can hide it (including a jump to the same field).
                    if steps is not None:
                        idx, steps = idx + 1, None
                    elif idx < len(route) and previous_area == route[idx][0]:
                        status = "obsolete-route"
                        print(f"  obsolete route: entry {idx + 1}/{len(route)} in {previous_area} was visited and left before its actions could start; actions unplayed", flush=True)
                    entry_expects_exit = False
                    pending = []  # answers queued in the previous visit must not dismiss a new field's windows
                    leg_goal, leg_best, leg_best_at, tries, tries_goal, detour = None, 0.0, now, 0, None, None
                    waited_for = None
                    last_move, waiting_until = now, 0.0
                    pos, facing = b["arrival"], b["arrival_facing"]
                c.send({"JoinArea": {"area": b["area"], "fingerprint": b["fingerprint"], "provenance": b["provenance"]}})
                mesh = mesh_for(area, args.cache, args.tool)
                legs, windows, locked = [], set(), False
                reached.append(area)
                busy_at = progress_at = now
                print(f"AREA {area}", flush=True)
                if status == "obsolete-route":
                    break
            elif k == "AreaSnapshot":
                for e in b["entities"]:
                    if e["id"] == entity:
                        pos, facing = e["pos"], e["facing"]
            elif k == "LiveCast":
                # (the bot is the lead: the cast's copy of it stands where the bot does)
                cast = [(n["name"], n["pos"]["x"], n["pos"]["y"]) for n in b["npcs"] if n["id"] != b.get("lead")]
            elif k == "LiveWindow":
                # A long scene can keep control for more than the stall budget while its dialogue advances.
                # Count a new window as progress, never the repeated cast/lock snapshots of a stuck scene.
                progress_at = now
                windows.add(b["window"])
                ask = b.get("ask")
                if args.verbose:
                    print(f"  WINDOW {b['window']} dialog={b['dialog']} ask={ask}", flush=True)
                if b["answerable"]:
                    pending.append((now + 0.2, {"LiveAnswer": {"window": b["window"], "choice": 0 if ask else None}}))
            elif k == "LiveWindowClosed":
                windows.discard(b["window"])
            elif k == "LiveControl":
                locked = b["locked"]
                if args.verbose:
                    print(f"  CONTROL locked={locked}", flush=True)
                if locked and steps is not None and not entry_expects_exit:
                    legs, steps = [], []  # a script took over: what was left of this entry is void
            elif k == "LivePlace":
                pos, facing = b["pos"], b["facing"]
                if not entry_expects_exit:
                    legs = []
                    if steps is not None:
                        steps = []
            elif k == "MoveRejected":
                # Refused, or ended beside someone solid: carry on towards the same corner from where the server says
                # we stand (the stuck check below walks round whoever is in the way).
                pos = b["corrected"]
                rejected += 1
                if args.verbose:
                    print(f"  corrected to {round(pos['x'])},{round(pos['y'])}", flush=True)
            elif k == "LiveMovie":
                print(f"  MOVIE {b['movie']}", flush=True)
                pending.append((now + 0.3, {"LiveMovieDone": {"movie": b["movie"]}}))
            elif k == "EncounterStart":
                in_battle = True
                battle_at = now
                print(f"  BATTLE start {b.get('formation_id')}", flush=True)
            elif k == "BattleEnd":
                in_battle = False
                print(f"  BATTLE end {b.get('outcome')}", flush=True)
            elif k == "Reject":
                print(f"REJECT {b}", flush=True)
                return 2
        if status == "obsolete-route":
            break
        for due, msg in [p for p in pending if p[0] <= now]:
            c.send(msg)
        pending = [p for p in pending if p[0] > now]
        busy = locked or bool(windows) or in_battle or bool(pending)
        if busy:
            busy_at = now
        if in_battle and now - battle_at < 600:
            progress_at = now  # a boss takes minutes: a battle under way is progress
        if idx >= len(route):
            status = "done"
            break
        if now - progress_at > args.stall:
            status = "stalled"
            break
        if pos is None or mesh is None or busy or now < waiting_until:
            time.sleep(0.02)
            continue
        if legs:
            if now - last_move >= 0.08:
                tx, ty = legs[0]
                dx, dy = tx - pos["x"], ty - pos["y"]
                d = math.hypot(dx, dy)
                on_detour = detour is not None and legs[0] == detour
                if leg_goal != legs[0] or d < leg_best - 1.0:
                    if not on_detour and legs[0] != tries_goal:
                        tries, tries_goal = 0, legs[0]
                    leg_goal, leg_best, leg_best_at = legs[0], d, now
                elif now - leg_best_at > 1.0:
                    # No nearer for a second: someone stands in the way (or on the corner itself).
                    leg_best_at = now
                    near = [f"{name} at {round(x)},{round(y)}" for name, x, y in cast if math.hypot(x - pos["x"], y - pos["y"]) < 160]
                    if on_detour:
                        legs.pop(0)
                        detour = None
                    elif near and waited_for != legs[0]:
                        # Someone is here: they may only be walking by. Give them a moment before going round.
                        waited_for = legs[0]
                        leg_best_at = now + 1.0
                        print(f"  waiting at {round(pos['x'])},{round(pos['y'])} for {', '.join(near)}", flush=True)
                    elif tries >= 4 and d < 90:
                        print(f"  gave up on corner {round(tx)},{round(ty)} from {round(pos['x'])},{round(pos['y'])}", flush=True)
                        legs.pop(0)
                    else:
                        side = (-1, 1)[tries % 2] * (45 + 35 * (tries // 2))
                        tries += 1
                        n = d or 1.0
                        detour = (pos["x"] - dy / n * side + dx / n * 10, pos["y"] + dx / n * side + dy / n * 10)
                        legs.insert(0, detour)
                        print(f"  in the way at {round(pos['x'])},{round(pos['y'])}: going round ({side:+d}); near: {', '.join(near) or 'nobody'}", flush=True)
                    time.sleep(0.01)
                    continue
                step = args.speed * min(now - last_move, 0.2)
                last_move = now
                if d <= step:
                    pos = {"x": tx, "y": ty, "z": pos["z"]}
                    legs.pop(0)
                else:
                    pos = {"x": pos["x"] + dx / d * step, "y": pos["y"] + dy / d * step, "z": pos["z"]}
                if d > 0:
                    facing = math.atan2(dy, dx)
                # stay on the level we walk on (layered fields): the height follows the triangle under us
                pos["z"] = mesh.height(mesh.locate(pos["x"], pos["y"], pos.get("z")), pos["x"], pos["y"])
                seq += 1
                c.send({"MoveIntent": {"seq": seq, "pos": pos, "facing": facing, "anim": "Walk" if legs else "Idle"}})
            time.sleep(0.01)
            continue
        field, entry_steps = route[idx]
        if steps is None:
            # start the entry once we stand in its field and the field has been idle for a moment
            if area != field or now - busy_at < 0.6:
                time.sleep(0.02)
                continue
            steps = list(entry_steps)
            # A route with an explicit gateway still needs to leave this field after any conversation on the way.
            # Other entries may intentionally end when a script takes over (ladders, jumps, story scenes).
            entry_expects_exit = any(s[0] == "gate" for s in entry_steps)
            print(f"[{idx + 1}/{len(route)}] {field}: {'; '.join(' '.join(s) for s in entry_steps)}", flush=True)
        if not steps:
            if now - busy_at >= 0.8 and now >= waiting_until:
                idx, steps = idx + 1, None
                progress_at = now
            else:
                time.sleep(0.02)
            continue
        step = steps.pop(0)
        last_move = now
        if args.verbose:
            print(f"    at {round(pos['x'])},{round(pos['y'])},{round(pos.get('z', 0))} tri {mesh.locate(pos['x'], pos['y'], pos.get('z'))}: {' '.join(step)}", flush=True)
        if step[0] == "tri":
            goal = int(step[1])
            legs = mesh.path(mesh.locate(pos["x"], pos["y"], pos.get("z")), goal) or [mesh.centre(goal)]
        elif step[0] == "xy":
            legs = plan(float(step[1]), float(step[2]), z=float(step[3]) if len(step) > 3 else None)
        elif step[0] == "to":
            legs = [(float(step[1]), float(step[2]))]
        elif step[0] == "gate":
            g = mesh.gates[int(step[1])]
            mx, my = (g["a"][0] + g["b"][0]) / 2, (g["a"][1] + g["b"][1]) / 2
            nx, ny = -(g["b"][1] - g["a"][1]), g["b"][0] - g["a"][0]
            n = math.hypot(nx, ny) or 1.0
            nx, ny = nx / n, ny / n
            if (pos["x"] - mx) * nx + (pos["y"] - my) * ny > 0:
                nx, ny = -nx, -ny
            before = (mx - nx * 30, my - ny * 30)
            legs = plan(*before, z=g.get("z")) + [(mx + nx * 40, my + ny * 40)]
            if args.verbose:
                print(f"    gate {g} legs {[(round(x), round(y)) for x, y in legs]}", flush=True)
        elif step[0] == "face":
            facing = (int(step[1]) - 64) / 256 * math.tau
            seq += 1
            c.send({"MoveIntent": {"seq": seq, "pos": pos, "facing": facing, "anim": "Idle"}})
            waiting_until = now + 0.15
        elif step[0] == "ok":
            c.send({"Interact": {}})
            waiting_until = now + 0.7
        elif step[0] == "wait":
            waiting_until = now + float(step[1])
    print(f"ROUTE {status} done={idx}/{len(route)} reached={reached[-1] if reached else '-'} areas={','.join(reached)}", flush=True)
    if status != "done":
        wanted = route[idx][0] if idx < len(route) else "-"
        print(f"  waiting for {wanted}: area={area} locked={locked} windows={sorted(windows)} battle={in_battle} pos={pos} legs={len(legs)} steps={steps}", flush=True)
    c.send("Leave")
    return 0 if status == "done" else 1


if __name__ == "__main__":
    sys.exit(main())
