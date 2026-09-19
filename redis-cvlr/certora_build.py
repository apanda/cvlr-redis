#!/usr/bin/env python3
"""Build redis-cvlr and emit the JSON the Certora Prover's build_script contract expects.

Shape copied from stellar-contracts/packages/*/certora_build.py, which is the only worked
example of this contract in the workspace.
"""
import argparse
import json
import subprocess
import sys
import tempfile
from pathlib import Path

SCRIPT_DIR = Path(__file__).resolve().parent

COMMAND = "just build"

PROJECT_DIR = SCRIPT_DIR
SOURCES = ["src/**/*.rs", "Cargo.toml"]
EXECUTABLES = "target/wasm32-unknown-unknown/release/redis_cvlr.wasm"

VERBOSE = False


def log(msg):
    if VERBOSE:
        print(msg, file=sys.stderr)


def run_command(command, to_stdout=False):
    log(f"Running {command!r} in {SCRIPT_DIR}")
    if to_stdout:
        result = subprocess.run(command, shell=True, text=True, cwd=SCRIPT_DIR)
        return None, None, result.returncode
    with tempfile.NamedTemporaryFile(delete=False, mode="w", prefix="certora_build_",
                                     suffix=".stdout") as out, \
         tempfile.NamedTemporaryFile(delete=False, mode="w", prefix="certora_build_",
                                     suffix=".stderr") as err:
        result = subprocess.run(command, shell=True, stdout=out, stderr=err, text=True,
                                cwd=SCRIPT_DIR)
        return out.name, err.name, result.returncode


def main():
    ap = argparse.ArgumentParser(description="Build redis-cvlr for the Certora Prover.")
    ap.add_argument("-o", "--output", metavar="FILE", help="write JSON to FILE")
    ap.add_argument("--json", action="store_true", help="dump JSON to stdout")
    ap.add_argument("-l", "--log", action="store_true", help="show cargo output")
    ap.add_argument("-v", "--verbose", action="store_true")
    args = ap.parse_args()

    global VERBOSE
    VERBOSE = args.verbose

    stdout_log, stderr_log, rc = run_command(COMMAND, args.log)
    if stdout_log:
        log(f"logs: {stdout_log} / {stderr_log}")

    data = {
        "project_directory": str(PROJECT_DIR),
        "sources": SOURCES,
        "executables": EXECUTABLES,
        "success": rc == 0,
        "return_code": rc,
        "log": {"stdout": stdout_log, "stderr": stderr_log},
    }

    if args.output:
        Path(args.output).write_text(json.dumps(data, indent=4))
    if args.json or not args.output:
        print(json.dumps(data, indent=4))

    sys.exit(0 if rc == 0 else 1)


if __name__ == "__main__":
    main()
