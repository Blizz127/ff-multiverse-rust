#!/usr/bin/env python3
"""Semantic checks for travel-smoke's isolated, genuinely registered v3 accounts.

Usage: travel-extended-check.py route WORK chain|escape ACCOUNT
       travel-extended-check.py relogin WORK ACCOUNT
Reads owned-run evidence only; writes one summary inside the temporary WORK directory.
The fresh-profile checks pin selected authored values, not arbitrary-byte PC equivalence.
The required route omits the optional Restore materia pickup: its SMTRA 53/AP 0
grant and bank13:31 result are not supported atomically, so no moment15 is saved.
The authored boss accepts moment14 directly and writes exact reactor bytes65/4.
"""

import json
import re
import sys
from pathlib import Path

SCENE_POINT = 4294967295
TEAM = "ff7/md1_1/team-talk"
FINALE = "ff7/nrthmk/reactor-finale"
POST = ("ff7/md8_4/post-escape", "ff7/md8_1/post-escape-station")
CONTINUATIONS = {TEAM, FINALE, *POST}
LANDINGS = {TEAM: "md1_2", FINALE: "md8_4", POST[0]: "md8_1"}
CHAIN = "md1_1 md1_2 nrthmk nmkin_1 elevtr1 nmkin_2 nmkin_3 nmkin_4 nmkin_5".split()
ESCAPE = CHAIN + "nmkin_4 nmkin_3 nmkin_2 elevtr1 nmkin_1 nrthmk".split()


def require(condition, message):
    if not condition:
        raise ValueError(message)


def log(work, name):
    return re.sub(r"\x1b\[[0-?]*[ -/]*[@-~]", "", (work / name).read_text())


def blob(text):
    require(isinstance(text, str) and re.fullmatch(r"(?:[0-9a-f]{2})+", text), "invalid progression hex")
    data = bytes.fromhex(text)
    require(len(data) >= 5 and data[0] == 3, "progression is not v3")
    count = int.from_bytes(data[3:5], "little")
    require(count <= 1278 and len(data) == 5 + 4 * count, "invalid v3 entry count/length")
    entries, previous = {}, (-1, -1)
    for offset in range(5, len(data), 4):
        bank, address, value, mask = data[offset:offset + 4]
        key = (bank, address)
        require(bank in (1, 3, 7, 11, 13) and not (bank == 1 and address < 2), "noncanonical v3 bank")
        require(key > previous and mask and not (value & ~mask), "noncanonical v3 ordering/value/mask")
        entries[key], previous = (value, mask), key
    return int.from_bytes(data[1:3], "little"), entries


def at(text, moment, reactor):
    actual, entries = blob(text)
    require(actual == moment, f"story moment {actual}, expected {moment}")
    require(entries.get((1, 225)) == (reactor, 255), f"reactor byte is not known {reactor}")
    return entries


def knowledge(before, after):
    old, new = blob(before)[1], blob(after)[1]
    require(all(new.get(key, (0, 0))[1] & mask == mask for key, (_, mask) in old.items()), "story lost known bank bits")


def fresh(text):
    entries = at(text, 1, 0)
    expected = {(1, 3): 50, (1, 4): 30, (1, 5): 10, (1, 6): 0,
                (1, 28): 251, (1, 29): 2, (13, 0): 1,
                # Opening PRTYE establishes Cloud alone after masking engine defaults.
                (3, 9): 0, (3, 10): 255, (3, 11): 255}
    expected.update({(1, address): 100 for address in range(80, 84)})
    require(all(entries.get(key) == (value, 255) for key, value in expected.items()), "fresh opening assignments differ")
    require(entries.get((1, 31)) == (0, 252), "fresh profile knows unsupported menu-lock bits")
    require(all(key not in entries for key in ((1, 16), (1, 24), (13, 104))), "fresh profile invents engine-owned bytes")


def joins(text):
    return re.findall(r"story: joined with progress ([0-9a-f]+)", text)


def advances(text):
    return [(int(point), summary, encoded) for point, summary, encoded in
            re.findall(r"story: advanced point=(\d+) (.*?) \(progress ([0-9a-f]+)\)", text)]


def server_steps(text, account):
    return [(area, int(point), summary.rstrip()) for area, point, summary in re.findall(
        rf"story advanced account={account} entity=\d+ area=(\S+) point=(\d+) ([^\n]*)", text)]


def scene_context(server, client, account, initial):
    # Story and scene notes have separate client queues. Only the server's stream
    # orders checkpoint publications; client notes independently prove every run finished.
    active, witnessed, finished = None, [], []
    for line in client.splitlines():
        start = re.search(r"scene: started run=(\d+) (\S+) as (owner|watcher)", line)
        end = re.search(r"scene: ended run=(\d+) outcome=(\w+)", line)
        if start:
            require(active is None, "overlapping scene starts")
            active = start.groups()
            witnessed.append(active)
        elif end:
            require(active is not None and end[1] == active[0] and end[2] == "Finished", "scene did not finish its matching run")
            finished.append(active[1])
            active = None
    require(active is None, "route ended during a scene")
    entities = re.findall(r"headless: joined [^\n]* \(entity (\d+)\)", client)
    require(len(entities) == 1, "expected one joined client entity")
    entity = int(entities[0])
    active, contexts, expected, previous = None, [], [], initial
    publications = iter(advances(client))
    following = {FINALE: POST[0], POST[0]: POST[1]}
    for line in server.splitlines():
        start = re.search(r"scene started run=(\d+) scene=(\S+) owner=(\d+) watchers=(\[[^]]*\])", line)
        if start and entity in [int(start[3]), *json.loads(start[4])]:
            role = "owner" if entity == int(start[3]) else "watcher"
            active = (start[1], start[2], role)
            expected.append(active)
            if start[2] == FINALE:
                expected.extend((start[1], scene, role) for scene in POST)
        step = re.search(rf"story advanced account={account} entity=(\d+) area=(\S+) point=(\d+) ", line)
        if not step:
            continue
        require(int(step[1]) == entity, "story account/entity does not match the joined bot")
        _, _, encoded = next(publications)
        context = None
        if int(step[3]) == SCENE_POINT and active:
            run, scene, role = active
            context = (scene, True)
            if step[2] == "Ff7/" + LANDINGS.get(scene, ""):
                require(encoded == previous, "landing checkpoint changed the prior scene's story")
                context = (scene, False)
                if scene in following:
                    active = (run, following[scene], role)
        else:
            active = None
        contexts.append(context)
        previous = encoded
    require(witnessed == expected, "client scene runs/roles differ from server runs and exact linked arrivals")
    return contexts, finished


def checkpoint_context(context, area, before, after):
    require(context is not None and context[0] in CONTINUATIONS, "checkpoint outside a reviewed scene")
    scene, active = context
    field = scene.split("/")[1] if active else LANDINGS[scene]
    require(area == f"Ff7/{field}", "checkpoint outside its scene or exact landing field")
    require(active or after == before, "landing publication changed story after the scene ended")
    if scene == POST[0]:
        require(after == before, "post-escape tunnel unexpectedly changed story")


def record_paths(work, account):
    directory = work / "server-state" / "progression" / str(account)
    return [directory / "Ff7.json", directory / "Ff7.character.json"]


def records(work, account):
    pair = [json.loads(path.read_text()) for path in record_paths(work, account)]
    require(all(pair[1].get(key) is None for key in ("checkpoint", "reservation", "scene_roster")), "completed account retains an active scene")
    return pair


def resources(character):
    # Receipt maps are private storage metadata, separate from CharacterProgress.
    fields = ("account_id", "game", "character", "level", "exp", "ap", "gil", "inventory")
    return {**{key: character[key] for key in fields}, "limit": character.get("limit", 0)}


def compare_steps(client, server):
    require(len(client) == len(server), f"client/server story counts differ: {len(client)}/{len(server)}")
    for (point, summary, _), (_, expected_point, expected_summary) in zip(client, server):
        require(point == expected_point and summary == expected_summary[:48], "client/server story point or summary differs")


def route(work, mode, account):
    require(mode in ("chain", "escape"), "route mode must be chain or escape")
    server = log(work, "server.log")
    clients = [log(work, f"client-{name}.log") for name in ("A", "B")]
    initial = [joins(client) for client in clients]
    require(all(len(items) == 1 for items in initial), "each fresh bot must receive exactly one initial story")
    for items in initial:
        fresh(items[0])
    require(initial[0] == initial[1], "fresh bots received different profiles")
    starts = [(int(i), int(f), int(m)) for i, f, m in re.findall(
        r"ordinary exact story battle started battle=(\d+) formation=(\d+) members=(\d+)", server)]
    require([(f, m) for _, f, m in starts] == [(300, 2), (324, 2)], "expected shared ordinary battles 300 then 324")
    require(len({i for i, _, _ in starts}) == 2, "battle ids were reused")
    ended = [(int(i), outcome) for i, outcome in re.findall(r"battle ended battle=(\d+) outcome=(\w+)", server)]
    require(ended == [(i, "Victory") for i, _, _ in starts], "battle outcomes do not match both starts")
    require(server.count("story battle outcome acknowledged") == 2, "expected two durable battle outcomes")
    for client in clients:
        seen = [(int(i), int(f)) for i, f in re.findall(r"battle#(\d+) started: formation (\d+),", client)]
        require(seen == [(i, f) for i, f, _ in starts], "bots did not share both battle ids/formations")
        verified = re.findall(r"battle#(\d+) ended in Victory [^\n]*; verified: the server's", client)
        require([int(i) for i in verified] == [i for i, _, _ in starts], "both victories must verify by owned-data replay")
    # Native nmkin_5 e1s0 requires moment<27, not the optional materia's moment15.
    # The boss sets1:226bit2 and1:225bit6, clears225bits3/4; rescue sets225bit5,
    # the return lift clears225bit0 and the finale restores it. Preserve every bit.
    trajectory = [("md1stin", 0, 6, 0, 0), ("md1_1", 0, 7, 0, 0), ("md1_2", 0, 8, 0, 0),
                  ("nrthmk", 0, 10, 0, 0), ("nmkin_1", 0, 11, 0, 0), ("elevtr1", 0, 12, 1, 0),
                  ("nmkin_2", 0, 13, 1, 0), ("nmkin_3", 0, 14, 1, 0), ("nmkin_5", 0, 27, 65, 4)]
    if mode == "escape":
        trajectory += [("nmkin_3", 0, 27, 97, 4), ("elevtr1", 1, 27, 96, 4), ("nrthmk", 1, 33, 97, 4), ("md8_1", SCENE_POINT, 36, 97, 4)]
    steps, told = server_steps(server, account), advances(clients[0])
    compare_steps(told, steps)
    contexts, finished = scene_context(server, clients[0], account, initial[0][0])
    required_scenes = [TEAM, FINALE, *POST] if mode == "escape" else [TEAM]
    require([scene for scene in finished if scene in CONTINUATIONS] == required_scenes, "reviewed route scenes did not finish in order")
    require(all(len(re.findall(rf"scene: started run=\d+ {re.escape(scene)} as owner", clients[0])) == 1 for scene in required_scenes), "leader did not own each reviewed scene once")
    require(len(contexts) == len(told), "story context count differs")
    previous = initial[0][0]
    transitions = []
    for (point, _, encoded), (area, _, _), context in zip(told, steps, contexts):
        knowledge(previous, encoded)
        moment, entries = blob(encoded)
        reactor, mask = entries.get((1, 225), (-1, 0))
        reactor2, mask2 = entries.get((1, 226), (-1, 0))
        require(mask == mask2 == 255, "route lost exact reactor knowledge")
        if point == SCENE_POINT:
            checkpoint_context(context, area, previous, encoded)
        if (moment, reactor, reactor2) != (blob(previous)[0], blob(previous)[1][(1, 225)][0], blob(previous)[1][(1, 226)][0]):
            if context in ((TEAM, True), (FINALE, True)):
                require(point == SCENE_POINT, "strict point scene used the legacy partial write")
            source_point = 0 if context == (TEAM, True) else 1 if context == (FINALE, True) else point
            transitions.append((area.removeprefix("Ff7/"), source_point, moment, reactor, reactor2))
        else:
            require(point == SCENE_POINT, "unexpected unchanged ordinary point publication")
        previous = encoded
    require(transitions == trajectory, f"route moment/reactor/area/point trajectory differs: {transitions!r}")
    story, character = records(work, account)
    require(story["blob"] == previous, "final leader wire/store story bytes differ")
    accounts = {int(path.parent.name) for path in (work / "server-state" / "progression").glob("*/Ff7.json")}
    require(account in accounts and len(accounts) == 2, "expected exactly two registered FF7 accounts")
    other = (accounts - {account}).pop()
    other_story, _ = records(work, other)
    other_steps, other_told = server_steps(server, other), advances(clients[1])
    compare_steps(other_told, other_steps)
    other_contexts, _ = scene_context(server, clients[1], other, initial[1][0])
    require(len(other_contexts) == len(other_told), "follower story context count differs")
    for (point, _, encoded), (area, _, _), context in zip(other_told, other_steps, other_contexts):
        blob(encoded)
        require(point == SCENE_POINT and encoded == initial[1][0], "follower's personal story advanced")
        checkpoint_context(context, area, encoded, encoded)
    require(other_story["blob"] == initial[1][0], "follower's stored fresh story changed")
    watched = len(re.findall(rf"scene: started run=\d+ {re.escape(FINALE)} as watcher", clients[1]))
    require(watched <= 1, "follower replayed the finale")
    routes = [ESCAPE + ["md8_4", "md8_1"], ESCAPE + (["md8_4", "md8_1"] if watched else [])] if mode == "escape" else [CHAIN, CHAIN]
    for client, expected in zip(clients, routes):
        require(re.findall(r"headless: now in Ff7/(\S+) ", client) == expected, "area route differs from the actual finale cohort")
    snapshot = {"account": account, "mode": mode, "blob": previous, "character": character, "other": other,
                "records": [path.read_text() for path in record_paths(work, account)],
                "other_records": [path.read_text() for path in record_paths(work, other)]}
    (work / "extended-route.json").write_text(json.dumps(snapshot, sort_keys=True) + "\n")


def relogin(work, account):
    snapshot = json.loads((work / "extended-route.json").read_text())
    require(snapshot["account"] == account and snapshot["mode"] == "escape", "wrong route snapshot for relogin")
    client, server = log(work, "client-A-2.log"), log(work, "server-2.log")
    require(joins(client) == [snapshot["blob"]], "relogin did not retain exact moment36 story bytes")
    require(at(snapshot["blob"], 36, 97).get((1, 226)) == (4, 255), "relogin lost authored reactor byte226")
    require(not server_steps(server, account) and not advances(client), "relogin repeated a story publication")
    story, character = records(work, account)
    require(story["blob"] == snapshot["blob"], "relogin changed story bytes")
    require(resources(character) == resources(snapshot["character"]), "post-escape replay changed character resources")
    require([path.read_text() for path in record_paths(work, account)] == snapshot["records"], "relogin rewrote leader records")
    require([path.read_text() for path in record_paths(work, snapshot["other"])] == snapshot["other_records"], "offline follower records changed")
    require(not re.search(r"scene: (?:started|ended)|scene (?:started|durably ended) run=|transaction acknowledged", client + server), "relogin replayed a scene or transaction")
    require(not re.search(r"battle(?:#| started battle=| ended battle=)", client + server), "post-escape relogin repeated a battle")


def main():
    if len(sys.argv) == 5 and sys.argv[1] == "route":
        route(Path(sys.argv[2]), sys.argv[3], int(sys.argv[4]))
    elif len(sys.argv) == 4 and sys.argv[1] == "relogin":
        relogin(Path(sys.argv[2]), int(sys.argv[3]))
    else:
        raise ValueError(__doc__.split("Reads")[0].strip())
    print("travel-extended-check: semantic checks passed")


if __name__ == "__main__":
    try:
        main()
    except (ValueError, OSError, KeyError, TypeError, IndexError, StopIteration) as error:
        print(f"travel-extended-check: {error}", file=sys.stderr)
        sys.exit(1)
