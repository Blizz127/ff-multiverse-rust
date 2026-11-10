#!/usr/bin/env python3
"""Two packaged clients share Jessie's ladder scene in the real FF7 reactor.

Owned library and optional Echo-S archive are read only. All writable state is fresh
and temporary. No GPU, device audio, live server or player save is touched.
"""
import argparse
import os
from pathlib import Path
import re
import shutil
import subprocess
import tempfile
import time


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("bin_dir", type=Path)
    parser.add_argument("--library", type=Path, default=Path.home() / ".local/share/ff-multiverse")
    parser.add_argument("--echo-s", type=Path)
    parser.add_argument("--timeout", type=int, default=90)
    parser.add_argument("--battle", action="store_true", help="continue through the materia step and shared Guard Scorpion battle")
    args = parser.parse_args()
    package, library = args.bin_dir.resolve(), args.library.resolve()
    if not (library / "library/sets").is_dir():
        parser.error("--library must contain the imported library/sets directory")
    work = Path(tempfile.mkdtemp(prefix="fmv-ladder-smoke.", dir="/tmp"))
    assert not work.is_relative_to(library)
    (work / "lib-state").mkdir()
    (work / "lib-state/library").symlink_to(library / "library", target_is_directory=True)
    env = dict(os.environ, FMV_STATE_DIR=str(work / "env"), FMV_AUDIO="null", NO_COLOR="1")
    for key in ["XDG_DATA_HOME", "XDG_STATE_HOME", "XDG_CONFIG_HOME", "XDG_CACHE_HOME"]:
        env[key] = str(work / key.lower())
    children, streams = [], []

    def spawn(name, binary, *extra):
        stream = (work / f"{name}.log").open("w")
        streams.append(stream)
        proc = subprocess.Popen([str(package / binary), *map(str, extra)], env=env,
                                stdout=stream, stderr=subprocess.STDOUT)
        children.append(proc)
        return proc

    def read(name):
        return (work / f"{name}.log").read_text(errors="replace")

    def until(predicate, seconds):
        deadline = time.monotonic() + seconds
        while time.monotonic() < deadline:
            if any(proc.poll() is not None for proc in children):
                raise AssertionError("a smoke process exited early")
            if predicate():
                return
            time.sleep(0.2)
        raise AssertionError("timed out")

    def stop(proc):
        if proc.poll() is None:
            proc.terminate()
            try:
                proc.wait(timeout=10)
            except subprocess.TimeoutExpired:
                proc.kill()
                proc.wait()

    try:
        spawn("server", "fmv-server", "--game", "ff7", "--area", "nmkin_2", "--bind", "127.0.0.1:0",
              "--state-dir", work / "server", "--ff7-library", work / "lib-state",
              "--debug-start-moment", "13", "--no-random-encounters",
              "--battle-level", "30", "--battle-speed", "0", "--battle-tick-rate", "240")
        until(lambda: "READY protocol=" in read("server"), 45)
        address = re.search(r"addr=([0-9.:]+)", read("server")).group(1)
        mods = ["--echo-s", args.echo_s.resolve()] if args.echo_s else ["--mods", "off"]
        route = "nmkin_3,nmkin_4,nmkin_5" if args.battle else "nmkin_3"
        for name, flags in [("leader", ["--party-invite-first-peer", "--story-advance"]),
                            ("member", ["--party-accept"])]:
            spawn(name, "fmv-client", "--game", "ff7", "--server", address, "--headless", "--name", name,
                  "--state-dir", work / name, "--ff7-library", work / "lib-state", "--duration", args.timeout + 10,
                  "--bot", "idle", "--travel", route, "--battle-auto", *flags, *mods)
        scene = "ff7/nmkin_3/jessie-ladder"
        until(lambda: all(re.search(r"scene: ended run=\d+ outcome=Finished windows=1", read(n))
                          for n in ["leader", "member"]), args.timeout)
        until(lambda: re.search(r"story saved account=\d+ bytes=\d+ progress=0[12]0e00[0-9a-f]*", read("server")), 10)
        if args.battle:
            until(lambda: all(re.search(r"battle#\d+ ended in Victory.*verified: the server's", read(n))
                              for n in ["leader", "member"]), args.timeout)
            until(lambda: re.search(r"story saved account=\d+ bytes=\d+ progress=0[12]1b00[0-9a-f]*", read("server")), 10)
        sl = read("server")
        matches = re.findall(rf"scene started run=(\d+) scene={scene} owner=(\d+) watchers=\[(\d+)\] preview=false writes_story=false", sl)
        assert len(matches) == 1, "one shared scene with one watcher"
        run, owner, watcher = matches[0]
        assert owner != watcher
        assert sl.count("moment=13->14") == 1, "exactly one existing story step"
        if not args.battle:
            assert not re.search(r"story battle started|countdown started", sl), "no unrelated story effects"
        for name, role in [("leader", "owner"), ("member", "watcher")]:
            cl = read(name)
            assert f"scene: started run={run} {scene} as {role} with 2 participant(s), script: original" in cl
            assert cl.count(f"scene: ended run={run} outcome=Finished windows=1") == 1
            assert "headless: now in Ff7/nmkin_3" in cl and "headless: peers in view: 1" in cl
            assert not re.search(r"BATTLE DIVERGENCE|NOT verified|gave up|outcome=(Skipped|TimedOut|Aborted)", cl)
        print("PASS: packaged server startup, two party clients in real nmkin_3, same ladder scene/run, one window each")
        print("PASS: one saved 13->14 step; shared scenes use original script even when Echo-S is supplied")
        if args.battle:
            battle_ids = re.findall(r"story battle started battle=(\d+) .*area=Ff7/nmkin_5 .*formation=324", sl)
            assert len(battle_ids) == 1, "one scripted Guard Scorpion"
            battle = battle_ids[0]
            assert re.search(rf"world: battle started battle={battle} formation=324 members=2 .*companions=2", sl)
            assert len(re.findall(rf"battle ended battle={battle} outcome=Victory", sl)) == 1
            assert sl.count("moment=14->15") == sl.count("moment=15->27") == sl.count("countdown started ") == 1
            for name in ["leader", "member"]:
                cl = read(name)
                assert f"battle#{battle} started: formation 324, party of 4" in cl
                assert re.search(rf"battle#{battle} ended in Victory.*message\(s\) replayed from this machine's own battle data and matched", cl)
                assert "headless: npc area=Ff7/nmkin_5 id=0 name=mtr " in cl
                assert "headless: 0 NPC(s) in Ff7/nmkin_5" in cl, "materia disappears on both clients"
            print("PASS: materia disappears on both clients, one shared boss victory with matched replays, one saved 15->27 step/countdown")
        for name in ["server", "leader", "member"]:
            for line in read(name).splitlines():
                if "scene started" in line or line.startswith("scene:") or "moment=" in line or "story saved" in line or "battle#" in line or "battle started" in line or "battle ended" in line:
                    print(f"{name}: {line}")
    except Exception:
        print(f"FAIL: logs retained at {work}")
        raise
    else:
        for proc in reversed(children):
            stop(proc)
        shutil.rmtree(work)
    finally:
        for proc in reversed(children):
            stop(proc)
        for stream in streams:
            stream.close()


if __name__ == "__main__":
    main()
