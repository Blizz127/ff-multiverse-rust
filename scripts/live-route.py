#!/usr/bin/env python3
"""Play a written route through live fields (`fmv-server --field-runtime`), protocol 23: the Disc 1 acceptance bot.

A route file has one line per field: `FIELD: STEP; STEP; ...`. Steps run in order each time the player is in FIELD:

    tri N        walk to the centre of walkmesh triangle N (planned over the walkmesh's links)
    gate N       walk across gateway slot N
    xy X Y       walk to the point (X, Y) of the field
    ok           press OK (the field's own scripts decide what it does)
    wait S       stand S seconds
    face DIR     face a field direction (0-255; 64 is +x)

Windows are answered (first choice, or `--answers`), movies acknowledged, battles left to the server's companions.
It prints the areas, windows, battles and story the server reports, and at the end `ROUTE reached=<field> ...`.

Walkmeshes come from `walkmesh_json` (game data: kept in --cache, outside the repository).
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
    route = {}
    for line in Path(path).read_text().splitlines():
        line = line.split("#", 1)[0].strip()
        if not line:
            continue
        field, steps = line.split(":", 1)
        route[field.strip()] = [s.split() for s in steps.split(";") if s.strip()]
    return route


class Mesh:
    def __init__(self, data):
        self.tris = [[tuple(v) for v in t["v"]] for t in data["triangles"]]
        self.links = [t["a"] for t in data["triangles"]]
        self.gates = data.get("gates", [])

    def centre(self, i):
        t = self.tris[i]
        return (sum(v[0] for v in t) / 3, sum(v[1] for v in t) / 3)

    def locate(self, x, y):
        best = None
        for i, t in enumerate(self.tris):
            (ax, ay, _), (bx, by, _), (cx, cy, _) = t
            d = (by - cy) * (ax - cx) + (cx - bx) * (ay - cy)
            if d == 0:
                continue
            l1 = ((by - cy) * (x - cx) + (cx - bx) * (y - cy)) / d
            l2 = ((cy - ay) * (x - cx) + (ax - cx) * (y - cy)) / d
            l3 = 1 - l1 - l2
            m = min(l1, l2, l3)
            if best is None or m > best[0]:
                best = (m, i)
        return best[1] if best else 0

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
    ap.add_argument("--seconds", type=float, default=600)
    ap.add_argument("--name", default="Router")
    ap.add_argument("--answers", default="")
    ap.add_argument("--speed", type=float, default=70.0, help="walking speed, field units per second")
    args = ap.parse_args()
    route = load_route(args.route)
    answers = [int(a) for a in args.answers.split(",") if a.strip()]
    c = livebot.Conn(args.addr)
    c.send({"Hello": {"protocol_version": livebot.PROTOCOL, "client_build": "live-route", "auth": {"Register": {"display_name": args.name}},
                      "character": {"tint": [80, 160, 220], "body": None, "slot": None}}})
    entity = area = mesh = None
    pos, facing, seq = None, 0.0, 0
    steps, legs = [], []
    locked = False
    windows = set()
    in_battle = False
    pending = []
    last_ping = last_move = time.monotonic()
    waiting_until = 0.0
    reached = []
    end = time.monotonic() + args.seconds
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
                steps = [list(s) for s in route.get(area, [])]
                legs, windows, locked = [], set(), False
                reached.append(area)
                print(f"AREA {area} steps={len(steps)}", flush=True)
            elif k == "AreaSnapshot":
                for e in b["entities"]:
                    if e["id"] == entity:
                        pos, facing = e["pos"], e["facing"]
            elif k == "LiveWindow":
                windows.add(b["window"])
                ask = b.get("ask")
                print(f"  WINDOW {b['window']} d={b['dialog']} ask={'-' if not ask else str(ask['first'])+'-'+str(ask['last'])}", flush=True)
                if b["answerable"]:
                    choice = (answers.pop(0) if answers else 0) if ask else None
                    pending.append((now + 0.3, {"LiveAnswer": {"window": b["window"], "choice": choice}}))
            elif k == "LiveWindowClosed":
                windows.discard(b["window"])
            elif k == "LiveControl":
                locked = b["locked"]
            elif k == "LivePlace":
                pos, facing = b["pos"], b["facing"]
                legs = []
            elif k == "MoveRejected":
                pos = b["corrected"]
                legs = []
            elif k == "LiveMovie":
                print(f"  MOVIE {b['movie']}", flush=True)
                pending.append((now + 0.5, {"LiveMovieDone": {"movie": b["movie"]}}))
            elif k == "EncounterStart":
                in_battle = True
                print(f"  BATTLE start {b.get('formation_id')}", flush=True)
            elif k == "BattleEnd":
                in_battle = False
                print(f"  BATTLE end {b.get('outcome')}", flush=True)
            elif k == "StoryUpdate" and b.get("advance"):
                print(f"  STORY {b['advance']}", flush=True)
            elif k == "Reject":
                print(f"REJECT {b}", flush=True)
                return 2
        for due, msg in [p for p in pending if p[0] <= now]:
            c.send(msg)
        pending = [p for p in pending if p[0] > now]
        if pos is None or locked or windows or in_battle or now < waiting_until:
            time.sleep(0.03)
            continue
        if legs:
            if now - last_move >= 0.1:
                tx, ty = legs[0]
                dx, dy = tx - pos["x"], ty - pos["y"]
                d = math.hypot(dx, dy)
                step = args.speed * (now - last_move)
                last_move = now
                if d <= step:
                    pos = {"x": tx, "y": ty, "z": pos["z"]}
                    legs.pop(0)
                else:
                    pos = {"x": pos["x"] + dx / d * step, "y": pos["y"] + dy / d * step, "z": pos["z"]}
                if d > 0:
                    facing = math.atan2(dy, dx)
                seq += 1
                c.send({"MoveIntent": {"seq": seq, "pos": pos, "facing": facing, "anim": "Walk" if legs else "Idle"}})
            time.sleep(0.02)
            continue
        if not steps:
            time.sleep(0.05)
            continue
        step = steps.pop(0)
        print(f"  STEP {' '.join(step)}", flush=True)
        if step[0] == "tri" and mesh:
            start = mesh.locate(pos["x"], pos["y"])
            legs = mesh.path(start, int(step[1])) or []
            if not legs:
                print(f"  no path to triangle {step[1]} from {start}", flush=True)
            last_move = now
        elif step[0] == "xy" and mesh:
            start = mesh.locate(pos["x"], pos["y"])
            goal = mesh.locate(float(step[1]), float(step[2]))
            legs = (mesh.path(start, goal) or []) + [(float(step[1]), float(step[2]))]
            last_move = now
        elif step[0] == "gate" and mesh:
            g = mesh.gates[int(step[1])]
            mx, my = (g["a"][0] + g["b"][0]) / 2, (g["a"][1] + g["b"][1]) / 2
            # through the line's middle, along its normal, to a point well past it
            nx, ny = -(g["b"][1] - g["a"][1]), g["b"][0] - g["a"][0]
            n = math.hypot(nx, ny) or 1.0
            nx, ny = nx / n, ny / n
            side = (pos["x"] - mx) * nx + (pos["y"] - my) * ny
            if side > 0:
                nx, ny = -nx, -ny
            before = (mx - nx * 30, my - ny * 30)
            start = mesh.locate(pos["x"], pos["y"])
            legs = (mesh.path(start, mesh.locate(*before)) or []) + [before, (mx + nx * 40, my + ny * 40)]
            last_move = now
        elif step[0] == "ok":
            c.send({"Interact": {}})
            waiting_until = now + 0.5
        elif step[0] == "wait":
            waiting_until = now + float(step[1])
        elif step[0] == "face":
            facing = (int(step[1]) - 64) / 256 * math.tau
            seq += 1
            c.send({"MoveIntent": {"seq": seq, "pos": pos, "facing": facing, "anim": "Idle"}})
    print(f"ROUTE reached={reached[-1] if reached else '-'} areas={','.join(reached)}", flush=True)
    c.send("Leave")
    return 0


if __name__ == "__main__":
    sys.exit(main())
