#!/usr/bin/env python3
"""Play a written route through live fields (`fmv-server --field-runtime`), protocol 23: the acceptance bot.

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

An entry is over when its steps are done and the field is idle again, or when the player lands in another field.
Windows are answered (first choice), movies acknowledged, battles left to the server's companions.
It prints what the server reports and ends with `ROUTE done=<entries played>/<entries> reached=<field> ...`
(exit status 0 only when every entry was played).

Walkmeshes come from the `walkmesh_json` example (game data: kept in --cache, outside the repository).
"""
import argparse
import importlib.util
import json
import math
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
    ap.add_argument("--speed", type=float, default=200.0, help="walking speed, field units per second")
    ap.add_argument("--stall", type=float, default=60.0, help="give up after this long without progress")
    ap.add_argument("--verbose", action="store_true")
    args = ap.parse_args()
    route = load_route(args.route)
    c = livebot.Conn(args.addr)
    c.send({"Hello": {"protocol_version": livebot.PROTOCOL, "client_build": "live-route", "auth": {"Register": {"display_name": args.name}},
                      "character": {"tint": [80, 160, 220], "body": None, "slot": None}}})
    entity = area = mesh = None
    pos, facing, seq = None, 0.0, 0
    legs = []
    locked = in_battle = False
    windows = set()
    pending = []
    idx, steps, entry_area = 0, None, None
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
            elif k in ("AreaManifest", "AreaChange"):
                area = b["area"]["key"]
                if k == "AreaChange":
                    pos, facing = b["arrival"], b["arrival_facing"]
                c.send({"JoinArea": {"area": b["area"], "fingerprint": b["fingerprint"], "provenance": b["provenance"]}})
                mesh = mesh_for(area, args.cache, args.tool)
                legs, windows, locked = [], set(), False
                reached.append(area)
                busy_at = progress_at = now
                print(f"AREA {area}", flush=True)
            elif k == "AreaSnapshot":
                for e in b["entities"]:
                    if e["id"] == entity:
                        pos, facing = e["pos"], e["facing"]
            elif k == "LiveWindow":
                windows.add(b["window"])
                ask = b.get("ask")
                if b["answerable"]:
                    pending.append((now + 0.2, {"LiveAnswer": {"window": b["window"], "choice": 0 if ask else None}}))
            elif k == "LiveWindowClosed":
                windows.discard(b["window"])
            elif k == "LiveControl":
                locked = b["locked"]
                if locked and steps is not None:
                    legs, steps = [], []  # a script took over: what was left of this entry is void
            elif k == "LivePlace":
                pos, facing = b["pos"], b["facing"]
                legs = []
                if steps is not None:
                    steps = []
            elif k == "MoveRejected":
                pos = b["corrected"]
                legs = []
                print(f"  rejected, back at {round(pos['x'])},{round(pos['y'])}", flush=True)
            elif k == "LiveMovie":
                print(f"  MOVIE {b['movie']}", flush=True)
                pending.append((now + 0.3, {"LiveMovieDone": {"movie": b["movie"]}}))
            elif k == "EncounterStart":
                in_battle = True
                print(f"  BATTLE start {b.get('formation_id')}", flush=True)
            elif k == "BattleEnd":
                in_battle = False
                print(f"  BATTLE end {b.get('outcome')}", flush=True)
            elif k == "Reject":
                print(f"REJECT {b}", flush=True)
                return 2
        for due, msg in [p for p in pending if p[0] <= now]:
            c.send(msg)
        pending = [p for p in pending if p[0] > now]
        busy = locked or bool(windows) or in_battle or bool(pending)
        if busy:
            busy_at = now
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
            steps, entry_area = list(entry_steps), area
            print(f"[{idx + 1}/{len(route)}] {field}: {'; '.join(' '.join(s) for s in entry_steps)}", flush=True)
        if area != entry_area:
            idx, steps = idx + 1, None  # the entry took us to another field
            progress_at = now
            continue
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
    c.send("Leave")
    return 0 if status == "done" else 1


if __name__ == "__main__":
    sys.exit(main())
