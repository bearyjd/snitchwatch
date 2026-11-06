#!/usr/bin/env python3
"""Generate the file-ownership table of the rounded-style plan.

Reads the selectable-text plan (batch table + pilots) and the restyle plan's
"Restyle batches" table, and rewrites the block between the OWNERSHIP markers
in the restyle plan. Usage:
  git show origin/feat/selectable-text-infra:docs/superpowers/plans/2026-10-30-selectable-text.md > sel.md
  python3 ownership_table.py sel.md docs/superpowers/plans/2026-10-30-rounded-ios-style.md
"""
import re
import sys

APPEND_ONLY = ["build.rs", "sources.rs"]


def selectable(text):
    owner = {}
    pilots = text.split("## Pilots")[1].split("## Rollout")[0]
    for f in re.findall(r"`(\w+\.qml)`", pilots):
        owner[f] = "pilot (#284)"
    for line in text.split("## Rollout batches")[1].splitlines():
        m = re.match(r"\|\s*(\d+)\s*\|[^|]*\|([^|]*)\|", line)
        if m:
            for name in re.findall(r"`(\w+)(?:\.qml)?`", m.group(2)):
                owner.setdefault(name + ".qml", f"batch {m.group(1)}")
                if name + ".qml" in owner and owner[name + ".qml"] != f"batch {m.group(1)}":
                    owner[name + ".qml"] += f" + batch {m.group(1)}"
    return owner


def restyle(text):
    owner = {}
    block = text.split("<!-- RESTYLE-BATCHES -->")[1].split("<!-- /RESTYLE-BATCHES -->")[0]
    for line in block.splitlines():
        m = re.match(r"\|\s*([A-F])\s*\|([^|]*)\|", line)
        if m:
            for f in re.findall(r"`([\w./]+)`", m.group(2)):
                owner[f] = m.group(1)
    return owner


def main(sel_path, plan_path):
    sel = selectable(open(sel_path).read())
    plan = open(plan_path).read()
    res = restyle(plan)
    for f in APPEND_ONLY:
        sel[f] = "every batch (append-only list)"
    files = sorted(set(sel) | set(res), key=lambda f: (f.count("/") > 0, f))
    rows = ["| File | Selectable text | Restyle | Order |", "|---|---|---|---|"]
    for f in files:
        s, r = sel.get(f), res.get(f)
        if s and r:
            if f in APPEND_ONLY:
                order = "append-only: keep both sides on rebase"
            else:
                order = f"selectable ({s}) merges first, then restyle {r} in the restyle PR"
        elif s:
            order = "selectable only"
        else:
            order = "restyle only, any time"
        rows.append(f"| `{f}` | {s or '-'} | {r or '-'} | {order} |")
    table = "\n".join(rows)
    new = re.sub(r"(<!-- OWNERSHIP -->\n).*?(\n<!-- /OWNERSHIP -->)",
                 lambda m: m.group(1) + table + m.group(2), plan, flags=re.S)
    open(plan_path, "w").write(new)
    print(f"{len(files)} files, {sum(1 for f in files if sel.get(f) and res.get(f))} overlap")


main(*sys.argv[1:3])
