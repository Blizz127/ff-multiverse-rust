"""Asset-free route timing checks. Run: python3 -m unittest discover -s scripts -p test_live_route.py."""
import contextlib
import importlib.util
import io
from pathlib import Path
import sys
import tempfile
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location("live_route", Path(__file__).with_name("live-route.py"))
route = importlib.util.module_from_spec(spec)
spec.loader.exec_module(route)


class Clock:
    def __init__(self):
        self.now = 0.0

    def monotonic(self):
        return self.now

    def sleep(self, seconds):
        self.now += seconds


def area_message(kind, name):
    body = {"area": {"key": name}, "fingerprint": "fixture", "provenance": {}}
    if kind == "AreaChange":
        body.update(arrival={"x": 0.0, "y": 0.0, "z": 0.0}, arrival_facing=0.0)
    return {kind: body}


class RouteTimingTests(unittest.TestCase):
    def play(self, commands, events=(), seconds=20, stall=2):
        clock = Clock()
        queued = [(0, {"Welcome": {"entity": 1}}), (0, area_message("AreaManifest", "a")),
                  (0, {"AreaSnapshot": {"entities": [{"id": 1, "pos": {"x": 0., "y": 0., "z": 0.}, "facing": 0.}]}})]
        queued.extend(events)
        queued.sort(key=lambda e: e[0])
        sent = []

        class Connection:
            def send(self, message):
                sent.append((clock.now, message))

            def poll(self):
                clock.now += .05
                messages = []
                while queued and queued[0][0] <= clock.now:
                    messages.append(queued.pop(0)[1])
                return messages

        mesh = route.Mesh({"triangles": [{"v": [[-10000, -10000, 0], [10000, -10000, 0], [0, 10000, 0]], "a": [-1, -1, -1]}]})
        output = io.StringIO()
        with tempfile.TemporaryDirectory(prefix="ffmv-route-unit.") as temporary:
            path = Path(temporary) / "route.txt"
            path.write_text(commands)
            argv = ["live-route.py", "unused:0", str(path), "--cache", temporary, "--tool", "unused",
                    "--seconds", str(seconds), "--stall", str(stall)]
            with patch.object(sys, "argv", argv), patch.object(route, "time", clock), \
                    patch.object(route.livebot, "Conn", return_value=Connection()), \
                    patch.object(route, "mesh_for", return_value=mesh), contextlib.redirect_stdout(output):
                code = route.main()
        return code, output.getvalue(), [(at, m["MoveIntent"]) for at, m in sent if isinstance(m, dict) and "MoveIntent" in m]

    def test_active_idle_pauses_during_a_scene_and_keeps_its_budget(self):
        code, output, moves = self.play("a: idle 3\na: to 20 0\n", [
            (1.2, {"LiveControl": {"locked": True}}),
            (2.5, {"LiveControl": {"locked": False}}),
        ])
        self.assertEqual(code, 0, output)
        self.assertGreater(moves[0][0], 4.8, "forced time must not consume the three idle seconds")
        self.assertEqual(moves[-1][1]["pos"]["x"], 20)

    def test_script_cancels_movement_but_preserves_its_trailing_idle(self):
        code, output, moves = self.play("a: to 500 0; to 1000 0; idle 2\na: to 20 0\n", [
            (1.2, {"LiveControl": {"locked": True}}),
            (1.3, {"LivePlace": {"pos": {"x": 10., "y": 0., "z": 0.}, "facing": 0.}}),
            (2.5, {"LiveControl": {"locked": False}}),
        ], stall=4)
        self.assertEqual(code, 0, output)
        resumed = [(at, move) for at, move in moves if at >= 1.2]
        self.assertGreater(resumed[0][0], 4.5)
        self.assertTrue(all(move["pos"]["x"] <= 20 for _, move in resumed), "cancelled movement must not resume")

    def test_dialogue_pauses_idle_without_a_control_lock(self):
        code, output, moves = self.play("a: idle 3\na: to 20 0\n", [
            (1.2, {"LiveWindow": {"window": 0, "dialog": 1, "answerable": False}}),
            (2.5, {"LiveWindowClosed": {"window": 0}}),
        ])
        self.assertEqual(code, 0, output)
        self.assertGreater(moves[0][0], 4.8)

    def test_field_transition_ends_the_started_entry_and_discards_old_idle(self):
        code, output, moves = self.play("a: idle 10\nb: to 20 0\n", [(1.2, area_message("AreaChange", "b"))])
        self.assertEqual(code, 0, output)
        self.assertLess(moves[0][0], 3)
        self.assertIn("done=2/2", output)

    def test_intentional_idle_can_exceed_stall_budget(self):
        code, output, _ = self.play("a: idle 5\n", stall=1.5)
        self.assertEqual(code, 0, output)

    def test_overall_time_limit_still_bounds_idle(self):
        code, output, _ = self.play("a: idle 50\n", seconds=3, stall=1.5)
        self.assertEqual(code, 1, output)
        self.assertIn("ROUTE time done=0/1", output)

    def test_idle_does_not_hide_a_locked_stall(self):
        code, output, _ = self.play("a: idle 5\n", [(1.2, {"LiveControl": {"locked": True}})], stall=1.5)
        self.assertEqual(code, 1, output)
        self.assertIn("ROUTE stalled done=0/1", output)

    def test_idle_clock_rejects_invalid_durations(self):
        for value in [-1, float("inf"), float("nan")]:
            with self.assertRaises(ValueError):
                route.IdleWait().start(value, 0)


if __name__ == "__main__":
    unittest.main()
