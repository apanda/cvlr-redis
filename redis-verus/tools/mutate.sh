#!/usr/bin/env bash
# Inject known bugs one at a time and confirm each breaks verification.
#
# A verification suite that has never rejected anything is not known to work. This is the
# Verus-leg counterpart of the injected-bug check in ../redis-cvlr/tests/difftest.rs.
#
# CAVEAT: the battery is hand-chosen with knowledge of what the proofs assert, so it is
# biased toward catchable bugs. A mechanical mutation pass measuring kill rate would be a
# stronger claim than this makes.
set -u
# NOTE: no `pipefail`. `cargo verus` exits non-zero when a proof fails, which is the
# outcome we are looking for -- with pipefail the detection pipeline always reports false.
cd "$(dirname "$0")/.."

REPO_ROOT="$(cd .. && pwd)"
VERUS_SRC="${VERUS_SRC:-$REPO_ROOT/../verus/source}"
VERUS_BIN="${VERUS_BIN:-$VERUS_SRC/target-verus/release}"
WORK=$(mktemp -d); trap 'rm -rf "$WORK"' EXIT
cp -R src Cargo.toml rust-toolchain.toml "$WORK/"
# absolute-ise the path deps so the copy resolves
python3 - "$WORK/Cargo.toml" "$VERUS_SRC" <<'PY'
import sys
p, src = sys.argv[1], sys.argv[2]
s = open(p).read()
open(p, 'w').write(s.replace("../../verus/source", src))
PY

verify_in_work () {
  ( cd "$WORK" && PATH="$VERUS_BIN:$PATH" cargo verus verify -p redis-verus 2>&1 )
}

baseline=$(verify_in_work | grep -aE "verification results:: [0-9]+ verified, 0 errors" | tail -1)
if [ -z "$baseline" ]; then echo "BASELINE FAILED -- fix the crate before mutating"; exit 1; fi
echo "baseline: $baseline"
echo
printf "%-44s %s\n" "MUTATION" "OUTCOME"
printf -- "---------------------------------------------------------------------\n"

fail=0
mutate () {
  local name="$1" file="$2" from="$3" to="$4"
  rm -rf "$WORK/src"; cp -R src "$WORK/src"   # restore pristine sources
  python3 - "$WORK/$file" "$from" "$to" <<'PY'
import sys
p, f, t = sys.argv[1], sys.argv[2], sys.argv[3]
s = open(p).read()
if s.count(f) != 1:
    sys.exit(9)
open(p, 'w').write(s.replace(f, t))
PY
  if [ $? -eq 9 ]; then printf "%-44s %s\n" "$name" "SKIPPED (anchor not unique)"; return; fi
  local out; out=$(verify_in_work)
  if echo "$out" | grep -qaE "^error: (postcondition|invariant|assertion)"; then
    printf "%-44s %s\n" "$name" "caught"
  else
    printf "%-44s %s\n" "$name" "*** NOT CAUGHT ***"; fail=1
  fi
}

mutate "aliasing: expire writes the wrong slot" src/model/step.rs \
  "w.slots.set(i, Slot { present: false, expire_at: NO_EXPIRE });" \
  "w.slots.set(if i > 0 { i - 1 } else { i }, Slot { present: false, expire_at: NO_EXPIRE });"

mutate "wf broken: delete leaves the TTL behind" src/model/cmd.rs \
  "w.slots.set(k, Slot { present: false, expire_at: NO_EXPIRE });" \
  "w.slots.set(k, Slot { present: false, expire_at: 1 });"

mutate "expire cycle deletes a non-due key" src/model/step.rs \
  "let due = w.slots[i].present
            && w.slots[i].expire_at >= 0
            && w.clock >= w.slots[i].expire_at;" \
  "let due = w.slots[i].present;"

mutate "lazy/active boundary: > becomes >=" src/model/expire.rs \
  "        w.clock > w.slots[k].expire_at
    }
}

/// Executable \`expireIfNeeded\` status." \
  "        w.clock >= w.slots[k].expire_at
    }
}

/// Executable \`expireIfNeeded\` status."

# The regression that actually shipped in the CVLR model: db.c:2950 says `when < 0`,
# i.e. ANY negative means no-expire, not just the -1 sentinel. It survived 20 000
# concrete executions per rule and 3000 differential schedules.
mutate "faithfulness: when < 0 reverted to == -1" src/model/expire.rs \
  "    } else if w.slots[k].expire_at < 0 {
        false
    } else {
        w.clock > w.slots[k].expire_at
    }
}

/// Executable" \
  "    } else if w.slots[k].expire_at == NO_EXPIRE {
        false
    } else {
        w.clock > w.slots[k].expire_at
    }
}

/// Executable"

echo
if [ $fail -eq 0 ]; then echo "all mutations caught"; else echo "SOME MUTATIONS NOT CAUGHT"; exit 1; fi
