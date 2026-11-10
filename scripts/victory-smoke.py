#!/usr/bin/env python3
"""One packaged server/client: boss victory dialogue, optionally followed by Jessie rescue.

Owned library and optional Echo-S archive are read only. Every writable path is fresh
throwaway state. No build, GPU, device audio, live server or player save is touched.
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
    parser.add_argument("--timeout", type=int, default=240)
    parser.add_argument("--rescue", action="store_true", help="also rescue Jessie and climb back to nmkin_2")
    args = parser.parse_args()
    package, library = args.bin_dir.resolve(), args.library.resolve()
    if not (library / "library/sets").is_dir():
        parser.error("--library must contain the imported library/sets directory")
    work = Path(tempfile.mkdtemp(prefix="fmv-victory-smoke.", dir="/tmp"))
    assert not work.is_relative_to(library)
    (work / "lib-state").mkdir()
    (work / "lib-state/library").symlink_to(library / "library", target_is_directory=True)
    env = dict(os.environ, FMV_STATE_DIR=str(work / "env"), FMV_AUDIO="null", NO_COLOR="1")
    for key in ["XDG_DATA_HOME", "XDG_STATE_HOME", "XDG_CONFIG_HOME", "XDG_CACHE_HOME"]:
        env[key] = str(work / key.lower())
    children, logs = [], []

    def spawn(name, binary, *extra):
        stream = (work / f"{name}.log").open("w")
        logs.append(stream)
        proc = subprocess.Popen([str(package / binary), *map(str, extra)], env=env,
                                stdout=stream, stderr=subprocess.STDOUT)
        children.append(proc)
        return proc

    def read(name):
        return (work / f"{name}.log").read_text(errors="replace")

    def until(predicate, process, seconds):
        deadline = time.monotonic() + seconds
        while time.monotonic() < deadline:
            if predicate():
                return
            if process.poll() is not None:
                raise AssertionError(f"process exited early: {process.returncode}")
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
        server = spawn("server", "fmv-server", "--game", "ff7", "--bind", "127.0.0.1:0", "--state-dir", work / "server",
                       "--ff7-library", work / "lib-state", "--debug-start-moment", "26", "--no-random-encounters",
                       "--battle-level", "30", "--battle-speed", "0", "--battle-tick-rate", "240")
        until(lambda: "READY protocol=" in read("server"), server, 45)
        address = re.search(r"addr=([0-9.:]+)", read("server")).group(1)
        route = "md1_1,md1_2,nrthmk,nmkin_1,elevtr1,nmkin_2,nmkin_3,nmkin_4,nmkin_5"
        if args.rescue:
            route += ",nmkin_4,nmkin_3,nmkin_2"
        mods = ["--echo-s", args.echo_s.resolve()] if args.echo_s else ["--mods", "off"]
        client = spawn("client", "fmv-client", "--game", "ff7", "--server", address, "--headless", "--name", "Victory",
                       "--state-dir", work / "client", "--ff7-library", work / "lib-state",
                       "--duration", args.timeout, "--bot", "circle", "--battle-auto", "--story-advance", "--travel", route, *mods)
        until(lambda: "scene: ended run=2 outcome=Finished windows=2" in read("client"), client, args.timeout)
        # Save completion is asynchronous; require its receipt as well as the on-screen continuation.
        saved_victory = r"story saved account=1 bytes=\d+ progress=0[12]1b00[0-9a-f]*"
        until(lambda: re.search(saved_victory, read("server")), server, 10)
        if args.rescue:
            until(lambda: "scene: ended run=3 outcome=Finished windows=3" in read("client")
                  and len(re.findall(r"headless: now in Ff7/nmkin_2\b", read("client"))) == 2, client, args.timeout)
            until(lambda: "story saved account=1 bytes=7 progress=021b000101e121" in read("server"), server, 10)
        stop(client)
        stop(server)
        sl, cl = read("server"), read("client")
        scene = "ff7/nmkin_5/guard-scorpion-victory"
        assert sl.count("story battle started ") == 1, "exactly one story battle"
        assert len(re.findall(r"battle ended battle=\d+ outcome=Victory", sl)) == 1, "one victory"
        assert sl.count("moment=26->27") == 1, "one victory step"
        assert len(re.findall(saved_victory, sl)) == (2 if args.rescue else 1), "stored steps at moment 27"
        assert sl.count("countdown started ") == 1, "one countdown"
        assert len(re.findall(rf"scene started .*scene={scene} ", sl)) == 1, "one victory scene"
        layer = "echo-s" if args.echo_s else "original"
        assert f"{scene} as owner with 1 participant(s), script: {layer}" in cl, "expected script layer"
        assert "scene: ended run=1 outcome=Finished windows=11" in cl, "pre-boss dialogue preserved"
        assert not re.search(r"[1-9][0-9]* divergence\(s\)|replay diverged", cl), "battle replay"
        assert sl.index("battle ended battle=") < sl.index("moment=26->27") < sl.index(f"scene={scene} ")
        print(f"PASS: one boss victory, one saved step/countdown, 11 pre-boss + 2 post-boss windows ({layer})")
        if args.rescue:
            rescue = "ff7/nmkin_3/jessie-rescue"
            assert len(re.findall(rf"scene started .*scene={rescue} ", sl)) == 1, "one rescue scene"
            assert f"{rescue} as owner with 1 participant(s), script: {layer}" in cl, "rescue script layer"
            assert sl.count("var1.225=1->33") == 1, "one existing rescue step"
            assert sl.count("story saved account=1 bytes=7 progress=021b000101e121") == 1, "one stored rescue step"
            assert sl.index(f"scene={rescue} ") < sl.index("var1.225=1->33"), "step follows scene"
            print(f"PASS: 3 Jessie rescue windows ({layer}), one saved rescue flag, return through ladders to nmkin_2")
        for line in cl.splitlines():
            if line.startswith("scene:"):
                print(line)
    except Exception:
        print(f"FAIL: logs retained at {work}")
        raise
    else:
        shutil.rmtree(work)
    finally:
        for proc in reversed(children):
            stop(proc)
        for stream in logs:
            stream.close()


if __name__ == "__main__":
    main()
