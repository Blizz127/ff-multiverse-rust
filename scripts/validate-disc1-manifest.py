#!/usr/bin/env python3
"""Check pinned Disc 1 instruction metadata against a user-supplied owned flevel.

Build once: cargo build -p fmv-ff7 --example manifest_check
Run: python3 scripts/validate-disc1-manifest.py /path/to/owned/flevel.lgp
Use --probe for a prebuilt example in another target directory. This wrapper never
builds, writes files or extracts assets. It invokes the repo's existing FF7 decoders.
Exit codes: 0 matches, 1 metadata mismatch, 2 input/decoder/tool error.
"""

import argparse
import json
import re
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent


def integer(value, low, high, label):
    if type(value) is not int or not low <= value <= high:
        raise ValueError(f"{label}: expected integer in {low}..{high}")
    return str(value)


def name(value, label, pattern=r"[A-Za-z0-9_]+"):
    if not isinstance(value, str) or not re.fullmatch(pattern, value):
        raise ValueError(f"{label}: expected a field/opcode/operand identifier")
    return value


def expectations(manifest):
    if manifest.get("schema") != "ffmv.disc1.route-manifest.v1":
        raise ValueError("unsupported manifest schema")
    rows = []
    for chapter in manifest["chapters"]:
        for site in chapter["pinned_sites"]:
            fields = [name(site["field"], "field"),
                      integer(site["field_id"], 0, 65535, "field_id"),
                      integer(site["entity"], 0, 255, "entity"),
                      integer(site["slot"], 0, 31, "slot"),
                      integer(site["offset"], 0, 2**32 - 1, "offset"),
                      name(site["op"], "op", r"[A-Za-z0-9_]+!?")]
            # Some anchors pin only the opcode; check every operand actually recorded.
            for operand in site.get("operands", []):
                operand_name = name(operand["name"], "operand name")
                bank = integer(operand["bank"], 0, 15, "operand bank")
                value = integer(operand["value"], -(2**31), 2**31 - 1, "operand value")
                fields.append(f"{operand_name}={bank}:{value}")
            rows.append("\t".join(fields))
    if not rows or len(rows) != manifest["inventory"]["pinned_instruction_references"]:
        raise ValueError("empty reference list or inventory reference count mismatch")
    return "\n".join(rows) + "\n"


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("flevel", type=Path, help="owned original flevel.lgp (read-only)")
    parser.add_argument("--manifest", type=Path, default=ROOT / "docs/disc1-route-manifest.json")
    parser.add_argument("--probe", type=Path, default=ROOT / "target/debug/examples/manifest_check")
    args = parser.parse_args()
    try:
        payload = expectations(json.loads(args.manifest.read_text()))
        if not args.flevel.is_file():
            raise ValueError("flevel path is not a file")
        if not args.probe.is_file():
            raise ValueError("probe missing; build with cargo build -p fmv-ff7 --example manifest_check, or pass --probe")
        return subprocess.run([str(args.probe.resolve()), str(args.flevel.resolve())], input=payload, text=True, check=False).returncode
    except (OSError, ValueError, KeyError, TypeError) as error:
        print(f"validate-disc1-manifest: {error}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    sys.exit(main())
