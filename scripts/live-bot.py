#!/usr/bin/env python3
"""A headless protocol-23 player for live fields (`fmv-server --field-runtime`): the route-driver bot.

It registers a fresh account, joins every area it is offered, dismisses every window it may answer (first choice for
questions, or the choices given with --answers), acknowledges movies at once, presses OK on a timer and walks toward
the cast when asked to, and prints one line per thing that happened:

    AREA <key>            the player entered an area
    WINDOW <w> d=<dialog> ask=<first-last|->   a script opened a window
    CAST <n>              the live cast changed (count)
    CONTROL locked=<0|1>
    PLACE x y             a script moved the player
    MOVIE <id>
    BATTLE ...            a battle started / ended
    STORY ...             a story update the server sent

Run it against a server on a temporary state directory only; it never reads game data.
"""
import argparse
import json
import math
import socket
import struct
import sys
import time

PROTOCOL = int(__import__("os").environ.get("FMV_PROTOCOL", "25"))


class Conn:
    def __init__(self, addr):
        host, port = addr.rsplit(":", 1)
        self.sock = socket.create_connection((host, int(port)), timeout=5)
        self.sock.settimeout(0.05)
        self.buf = b""

    def send(self, msg):
        payload = json.dumps(msg, separators=(",", ":")).encode()
        self.sock.sendall(struct.pack("<I", len(payload)) + payload)

    def poll(self):
        out = []
        try:
            chunk = self.sock.recv(1 << 20)
            if not chunk:
                raise ConnectionError("closed")
            self.buf += chunk
        except socket.timeout:
            pass
        while len(self.buf) >= 4:
            (n,) = struct.unpack("<I", self.buf[:4])
            if len(self.buf) < 4 + n:
                break
            out.append(json.loads(self.buf[4 : 4 + n]))
            self.buf = self.buf[4 + n :]
        return out


def kind(msg):
    return next(iter(msg)) if isinstance(msg, dict) else msg


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("addr")
    ap.add_argument("--name", default="LiveBot")
    ap.add_argument("--seconds", type=float, default=60)
    ap.add_argument("--press-every", type=float, default=0, help="press OK every N seconds (0: never)")
    ap.add_argument("--answers", default="", help="comma-separated answers for questions, in order")
    ap.add_argument("--approach", action="store_true", help="walk toward the nearest cast member and press OK there")
    ap.add_argument("--token-file", help="log in with the token in this file; register and save it there when it is missing")
    args = ap.parse_args()
    answers = [int(a) for a in args.answers.split(",") if a.strip()]

    c = Conn(args.addr)
    import os
    token = open(args.token_file).read().strip() if args.token_file and os.path.exists(args.token_file) else None
    auth = {"Token": token} if token else {"Register": {"display_name": args.name}}
    c.send({"Hello": {"protocol_version": PROTOCOL, "client_build": "live-bot", "auth": auth, "character": {"tint": [200, 120, 60], "body": None, "slot": None}}})
    entity, pos, facing, area, seq = None, {"x": 0.0, "y": 0.0, "z": 0.0}, 0.0, None, 0
    cast, locked, last_press, end = [], False, time.monotonic(), time.monotonic() + args.seconds
    pending = []  # (due, msg)
    last_ping = 0.0
    while time.monotonic() < end:
        if time.monotonic() - last_ping >= 1.0:
            last_ping = time.monotonic()
            c.send({"Ping": {"nonce": int(last_ping * 1000)}})
        for msg in c.poll():
            k = kind(msg)
            body = msg[k] if isinstance(msg, dict) else None
            if k == "Welcome":
                entity = body["entity"]
                if args.token_file and body.get("issued_token"):
                    with open(args.token_file, "w") as f:
                        f.write(body["issued_token"])
                print(f"WELCOME entity={entity}", flush=True)
            elif k in ("AreaManifest", "AreaChange"):
                area = body["area"]
                if k == "AreaChange":
                    pos = body["arrival"]
                    facing = body["arrival_facing"]
                c.send({"JoinArea": {"area": body["area"], "fingerprint": body["fingerprint"], "provenance": body["provenance"]}})
                print(f"AREA {area['key']}", flush=True)
            elif k == "AreaSnapshot":
                for e in body["entities"]:
                    if e["id"] == entity:
                        pos, facing = e["pos"], e["facing"]
                cast = body.get("npcs", [])
            elif k == "LiveCast":
                cast = body["npcs"]
                print(f"CAST {len(cast)} " + " ".join(f"{n['name']}@{round(n['pos']['x'])},{round(n['pos']['y'])}" for n in cast[:8]), flush=True)
            elif k == "LiveWindow":
                ask = body.get("ask")
                print(f"WINDOW {body['window']} d={body['dialog']} ask={(str(ask['first'])+'-'+str(ask['last'])) if ask else '-'} answerable={body['answerable']}", flush=True)
                if body["answerable"]:
                    choice = (answers.pop(0) if answers else 0) if ask else None
                    pending.append((time.monotonic() + 0.2, {"LiveAnswer": {"window": body["window"], "choice": choice}}))
            elif k == "LiveWindowClosed":
                print(f"WINDOW-CLOSED {body['window']}", flush=True)
            elif k == "LiveControl":
                locked = body["locked"]
                print(f"CONTROL locked={int(locked)}", flush=True)
            elif k == "LivePlace":
                pos, facing = body["pos"], body["facing"]
                print(f"PLACE {round(pos['x'])} {round(pos['y'])}", flush=True)
            elif k == "LiveMovie":
                print(f"MOVIE {body['movie']}", flush=True)
                pending.append((time.monotonic() + 0.5, {"LiveMovieDone": {"movie": body["movie"]}}))
            elif k == "EncounterStart":
                print(f"BATTLE start formation={body.get('formation_id')} battle={body.get('battle_id')}", flush=True)
            elif k == "BattleEnd":
                print(f"BATTLE end outcome={body.get('outcome')}", flush=True)
            elif k == "StoryUpdate":
                print(f"STORY {body.get('advance')}", flush=True)
            elif k == "Reject":
                print(f"REJECT {body}", flush=True)
                return 2
            elif k == "Ping":
                pass
        now = time.monotonic()
        for due, m in [p for p in pending if p[0] <= now]:
            c.send(m)
        pending = [p for p in pending if p[0] > now]
        if args.press_every and now - last_press >= args.press_every and not locked:
            last_press = now
            if args.approach and cast:
                target = min(cast, key=lambda n: math.dist((n["pos"]["x"], n["pos"]["y"]), (pos["x"], pos["y"])))
                dx, dy = target["pos"]["x"] - pos["x"], target["pos"]["y"] - pos["y"]
                d = math.hypot(dx, dy)
                if d > 60:
                    step = min(40.0, d - 50)
                    pos = {"x": pos["x"] + dx / d * step, "y": pos["y"] + dy / d * step, "z": pos["z"]}
                facing = math.atan2(dy, dx)
                seq += 1
                c.send({"MoveIntent": {"seq": seq, "pos": pos, "facing": facing, "anim": "Walk"}})
            c.send({"Interact": {}})
    c.send("Leave")
    return 0


if __name__ == "__main__":
    sys.exit(main())
