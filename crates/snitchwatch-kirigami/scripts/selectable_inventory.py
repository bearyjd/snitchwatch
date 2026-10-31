#!/usr/bin/env python3
"""Classify every text item under qml/ for the selectable-text rollout.

Usage: scripts/selectable_inventory.py [--files]
Prints per-class counts (and, with --files, a per-file breakdown). A
heuristic, not a proof: it tracks the QML type stack by brace depth and
classifies each Label / Text / Heading / TextArea / TextField by its own
properties and its ancestors. Classes are defined in
docs/superpowers/plans/2026-10-30-selectable-text.md.
"""
import collections
import pathlib
import re
import sys

QML = pathlib.Path(__file__).resolve().parent.parent / "qml"
TEXT_TYPES = {"Controls.Label", "Label", "Kirigami.Heading", "Text"}
FIELD_TYPES = {"Controls.TextArea", "Controls.TextField", "TextArea", "TextField",
               "Kirigami.SearchField", "Kirigami.ActionTextField", "TextEdit"}
ROW_TYPES = {"NavigableRow", "Controls.ItemDelegate", "ItemDelegate", "Controls.SwipeDelegate",
             "Kirigami.AbstractListItem", "FocusOrderListView", "ConnectionRow"}
CONTROL_TYPES = {"Controls.Button", "Button", "GuardedButton", "GuardedToolButton", "RuleButton",
                 "Controls.ToolButton", "Controls.TabButton", "Controls.MenuItem", "Controls.Menu",
                 "Controls.CheckBox", "Controls.RadioButton", "GuardedRadioButton", "Controls.Switch",
                 "GuardedSwitch", "Controls.ComboBox", "Controls.ToolTip", "Kirigami.Action",
                 "Controls.CheckDelegate", "Controls.SwitchDelegate", "Controls.RadioDelegate",
                 "SegmentedChoice", "DecideLaterButton", "HoverTip", "Controls.Dialog",
                 "Controls.Popup"}
# Files whose root item is itself a clickable list row (a delegate).
ROW_FILES = {"ConnectionRow", "LearningReviewRow"}
KIRIGAMI_E = {"Kirigami.InlineMessage", "Kirigami.PlaceholderMessage"}
KIRIGAMI_OWNED = {"Kirigami.InlineMessage", "Kirigami.PlaceholderMessage", "Kirigami.PageRow",
                  "Kirigami.GlobalDrawer", "Kirigami.OverlayDrawer", "Kirigami.AbstractApplicationHeader",
                  "Kirigami.Page", "Kirigami.ScrollablePage", "Kirigami.OverlaySheet",
                  "SizedOverlaySheet", "GlobalDrawer"}


def strip(src):
    """Blank out comments and string contents, keeping offsets and newlines."""
    out, i, n = [], 0, len(src)
    while i < n:
        c = src[i]
        if src.startswith("//", i):
            j = src.find("\n", i)
            j = n if j < 0 else j
            out.append(" " * (j - i)); i = j
        elif src.startswith("/*", i):
            j = src.find("*/", i); j = n if j < 0 else j + 2
            out.append(re.sub(r"[^\n]", " ", src[i:j])); i = j
        elif c in "\"'":
            j = i + 1
            while j < n and src[j] != c:
                j += 2 if src[j] == "\\" else 1
            out.append(c + re.sub(r"[^\n]", " ", src[i + 1:j]) + c); i = j + 1
        else:
            out.append(c); i += 1
    return "".join(out)


OPEN = re.compile(r"([A-Z][A-Za-z0-9_]*(?:\.[A-Z][A-Za-z0-9_]*)*)\s*\{$")


def items(src):
    """Yield (type, ancestor records, body) for every typed block; a record is
    (type, body) so a caller can look at what an ancestor does."""
    code = strip(src)
    stack = []  # (type, start of body)
    for i, c in enumerate(code):
        if c == "{":
            m = OPEN.search(code[max(0, i - 80):i + 1])
            stack.append((m.group(1) if m else None, i + 1))
        elif c == "}" and stack:
            t, start = stack.pop()
            if t:
                yield t, [s[0] for s in stack if s[0]], code[start:i], i


def own(body):
    """The block's own lines (children blocks removed)."""
    depth, out = 0, []
    for ch in body:
        if ch == "{":
            depth += 1
        elif ch == "}":
            depth -= 1
        elif depth == 0:
            out.append(ch)
    return "".join(out)


CLICK = re.compile(r"\bonClicked\b|\bonTapped\b|\bTapHandler\b|\bMouseArea\b|\bonPressed\b")


def classify(t, anc, body, clickable_anc=False):
    if t in FIELD_TYPES:
        return "F"
    mine = own(body)
    if any(a in CONTROL_TYPES for a in anc):
        return "D"
    if any(a in KIRIGAMI_OWNED and a not in ("Kirigami.ScrollablePage", "Kirigami.Page") for a in anc
           if a in ("Kirigami.InlineMessage", "Kirigami.PlaceholderMessage", "Kirigami.GlobalDrawer",
                    "GlobalDrawer", "Kirigami.PageRow")):
        return "E"
    if re.search(r"^\s*elide\s*:\s*Text\.Elide", mine, re.M):
        return "B"
    if any(a in ROW_TYPES for a in anc) or clickable_anc:
        return "C"
    return "A"


def main():
    counts, per_file = collections.Counter(), collections.defaultdict(collections.Counter)
    for f in sorted(QML.glob("*.qml")):
        src = f.read_text()
        code = strip(src)
        recs = list(items(src))
        for t, anc, body, end in recs:
            if t in KIRIGAMI_E:
                counts["E"] += 1
                per_file[f.name]["E"] += 1
            if t in TEXT_TYPES or t in FIELD_TYPES:
                start = end - len(body)
                # clickable_anc: an enclosing delegate/Item block that handles clicks
                clickable = f.stem in ROW_FILES or any(
                    r[3] > end and r[3] - len(r[2]) < start and CLICK.search(r[2])
                    and re.search(r"delegate\s*:\s*[A-Za-z.]+\s*\{$", code[:r[3] - len(r[2])])
                    for r in recs)
                k = classify(t, anc, body, clickable)
                counts[k] += 1
                counts["type:" + ("Heading" if t == "Kirigami.Heading" else t.split(".")[-1])] += 1
                per_file[f.name][k] += 1
    total = sum(v for k, v in counts.items() if len(k) == 1 and k != "E")
    print(f"files with text items: {len(per_file)}   Label/Text/Heading/field items: {total}")
    for k in "ABCDF":
        print(f"  class {k}: {counts[k]}")
    print(f"  class E: {counts['E']} Kirigami-owned components (InlineMessage, PlaceholderMessage, ...)")
    print("  by type: " + ", ".join(f"{k[5:]}={v}" for k, v in sorted(counts.items()) if k.startswith("type:")))
    if "--files" in sys.argv:
        for name, c in sorted(per_file.items()):
            print(f"{name:34s} " + " ".join(f"{k}={c[k]}" for k in "ABCDEF" if c[k]))


main()
