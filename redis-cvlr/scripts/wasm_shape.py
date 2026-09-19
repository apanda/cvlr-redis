#!/usr/bin/env python3
"""Print a wasm module's imports and exports.

Used to confirm, without a Prover, that a build produced the artifact shape CVLR needs:
imports ONLY from module "env" and all named CVT_*, exports "memory" plus every #[rule]
symbol by name. A rule that is not in the export list will never be found by the Prover.
"""
import sys

def u32(d, p):
    r = s = 0
    while True:
        b = d[p]; p += 1
        r |= (b & 0x7f) << s; s += 7
        if not b & 0x80:
            return r, p

def main(path):
    d = open(path, 'rb').read()
    if d[:4] != b'\x00asm':
        sys.exit(f"{path}: not a wasm module")
    p = 8
    imports, exports = [], []
    while p < len(d):
        sid = d[p]; p += 1
        size, p = u32(d, p)
        end = p + size
        if sid == 2:
            n, q = u32(d, p)
            for _ in range(n):
                l, q = u32(d, q); mod = d[q:q+l].decode(); q += l
                l, q = u32(d, q); nm = d[q:q+l].decode(); q += l
                kind = d[q]; q += 1
                if kind == 0:
                    _, q = u32(d, q)
                elif kind == 1:
                    q += 1; fl = d[q]; q += 1
                    _, q = u32(d, q)
                    if fl: _, q = u32(d, q)
                elif kind == 2:
                    fl = d[q]; q += 1
                    _, q = u32(d, q)
                    if fl: _, q = u32(d, q)
                elif kind == 3:
                    q += 2
                imports.append((mod, nm))
        elif sid == 7:
            n, q = u32(d, p)
            for _ in range(n):
                l, q = u32(d, q); nm = d[q:q+l].decode(); q += l
                q += 1
                _, q = u32(d, q)
                exports.append(nm)
        p = end

    mods = {}
    for m, n in imports:
        mods.setdefault(m, []).append(n)
    print(f"{path}\n")
    print("IMPORTS")
    for m, ns in sorted(mods.items()):
        flag = "" if m == "env" else "   <-- UNEXPECTED MODULE"
        print(f"  {m} ({len(ns)}){flag}")
        for n in sorted(ns):
            print(f"    {n}")
    print("\nEXPORTS")
    for n in exports:
        print(f"  {n}")
    bad = [m for m in mods if m != "env"]
    non_cvt = [n for m, n in imports if m == "env" and not n.startswith("CVT_")]
    rules = [e for e in exports if e != "memory" and not e.startswith("__")]
    print()
    if bad:
        print(f"FAIL: imports from non-env module(s): {bad}")
    if non_cvt:
        print(f"WARN: non-CVT_ imports from env: {non_cvt}")
    print(f"{len(rules)} exported rule symbol(s).")
    return 1 if bad else 0

if __name__ == "__main__":
    sys.exit(main(sys.argv[1] if len(sys.argv) > 1 else sys.exit("usage: wasm_shape.py FILE")))
