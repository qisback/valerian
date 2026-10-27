#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
# SPDX-License-Identifier: GPL-3.0-or-later
# See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.
"""Check every `valerian ...` command in the book against the command line reference.

    python check_book_commands.py [BOOK_SRC]

The reference (reference/cli.md) is generated from the clap definition and CI keeps
it current (`cargo xtask docs --check`), so it is the same text as `valerian --help`.
Every fenced `sh` block in the book is scanned; each `valerian SUBCOMMAND ...` line
must name a real subcommand, use only options that subcommand has, give an option
that takes a value its value (and a legal one for an option with a fixed set of
values), and name the required options and arguments. Exit status 1 lists every
mismatch. Standard library only.
"""
import pathlib
import re
import shlex
import sys

OPTION = re.compile(r"^\* `([^`]+)`")
CHOICE = re.compile(r"^  \* `([^`]+)`")
HEADING = re.compile(r"^### `valerian (\w[\w-]*)`")
USAGE = re.compile(r"^valerian (\w[\w-]*) (.*)$")


def load_reference(path):
    """subcommand -> {"flags": {flag: (n_values, choices|None)}, "required": [flags], "positional": bool}"""
    subs, cur, last, in_usage = {}, None, None, False
    for line in pathlib.Path(path).read_text(encoding="utf-8").splitlines():
        m = HEADING.match(line)
        if m:
            cur = subs.setdefault(m.group(1), {"flags": {"-h": (0, None), "--help": (0, None)}, "required": [], "positional": False})
            last = None
            continue
        if cur is None:
            continue
        m = USAGE.match(line)
        if m:
            cur["positional"] = bool(re.search(r"<[A-Z_]+>", re.sub(r"--?[\w-]+ <[A-Z_]+>", "", m.group(2))))
            continue
        m = CHOICE.match(line)
        if m and last:
            cur["flags"][last][1].add(m.group(1))
            continue
        m = OPTION.match(line)
        if m and "(" not in m.group(1)[:1]:
            spec = m.group(1)
            names = re.findall(r"(?<![\w<])(--?[A-Za-z][\w-]*)", spec)
            if not names:
                last = None
                continue
            n_values = len(re.findall(r"<[^>]+>", spec))
            for n in names:
                cur["flags"][n] = (n_values, set())
            last = names[-1]
            if "(required" in line:
                cur["required"].append(names[-1])
    for s in subs.values():
        for k, (n, ch) in list(s["flags"].items()):
            s["flags"][k] = (n, ch or None)
    return subs


def blocks(text):
    """Yield (first line number, text) of each fenced sh block."""
    lines, i = text.splitlines(), 0
    while i < len(lines):
        m = re.match(r"^```(sh|bash|console|shell)\s*$", lines[i])
        if m:
            start, body = i + 2, []
            i += 1
            while i < len(lines) and not lines[i].startswith("```"):
                body.append(lines[i])
                i += 1
            yield start, "\n".join(body)
        i += 1


def commands(body):
    """Logical command lines with backslash continuations joined; comments dropped."""
    out, buf = [], ""
    for raw in body.splitlines():
        line = re.sub(r"\s+#.*$", "", raw) if not raw.lstrip().startswith("#") else ""
        if line.endswith("\\"):
            buf += line[:-1] + " "
            continue
        out.append(buf + line)
        buf = ""
    return [c for c in out if c.strip()]


def check_command(cmd, subs):
    try:
        tokens = shlex.split(cmd)
    except ValueError as e:
        return [f"cannot parse: {e}"]
    errs = []
    # Split on shell operators; an invocation is a segment whose command word is `valerian`
    # (after any VAR=value words), so a path to the binary in an argument is not one.
    segments, cur = [], []
    for t in tokens:
        if t in ("|", "&&", ";", "||", ">", ">>"):
            segments.append(cur)
            cur = []
        else:
            cur.append(t)
    segments.append(cur)
    for seg in segments:
        while seg and re.fullmatch(r"[A-Za-z_]\w*=.*", seg[0]):
            seg = seg[1:]
        if not seg or pathlib.PurePosixPath(seg[0]).name != "valerian":
            continue
        rest = seg[1:]
        if not rest or rest[0] in ("-h", "--help", "-V", "--version", "help"):
            continue
        sub = rest[0]
        if sub not in subs:
            errs.append(f"no such subcommand `{sub}`")
            continue
        spec, seen, positional, j = subs[sub], set(), 0, 1
        while j < len(rest):
            t = rest[j]
            j += 1
            if t == "--":
                positional += len(rest) - j
                break
            if t.startswith("-") and len(t) > 1 and not re.fullmatch(r"-\d+(\.\d+)?", t):
                name, eq, val = t.partition("=")
                if name not in spec["flags"]:
                    errs.append(f"`valerian {sub}` has no option `{name}`")
                    continue
                n, choices = spec["flags"][name]
                seen.add(name)
                vals = [val] if eq else []
                while len(vals) < n and j < len(rest):
                    vals.append(rest[j])
                    j += 1
                if len(vals) < n:
                    errs.append(f"`{name}` of `valerian {sub}` needs {n} value(s)")
                if choices and vals and vals[0] not in choices:
                    errs.append(f"`{name} {vals[0]}` is not one of {', '.join(sorted(choices))}")
            else:
                positional += 1
        for r in spec["required"]:
            names = {r} | {k for k, v in spec["flags"].items() if v == spec["flags"][r]}
            if not names & seen:
                errs.append(f"`valerian {sub}` needs `{r}`")
        if spec["positional"] and positional == 0 and sub != "info":
            errs.append(f"`valerian {sub}` needs its input argument")
    return errs


def main():
    src = pathlib.Path(sys.argv[1] if len(sys.argv) > 1 else pathlib.Path(__file__).resolve().parents[1] / "docs/book/src")
    subs = load_reference(src / "reference/cli.md")
    if not subs:
        print("no subcommands found in reference/cli.md", file=sys.stderr)
        return 1
    bad = checked = 0
    for md in sorted(src.rglob("*.md")):
        if md.name == "cli.md":
            continue
        for first, body in blocks(md.read_text(encoding="utf-8")):
            for cmd in commands(body):
                if "valerian" not in cmd:
                    continue
                checked += 1
                for e in check_command(cmd, subs):
                    bad += 1
                    print(f"{md.relative_to(src)}:{first}: {e}\n    {cmd.strip()}")
    print(f"{checked} commands checked against {len(subs)} subcommands, {bad} problem(s)")
    return 1 if bad else 0


if __name__ == "__main__":
    sys.exit(main())
