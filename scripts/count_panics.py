#!/usr/bin/env python3
"""Count panic sites in non-test Rust code.

Method:
  1. Lex each .rs file. Blank out comments, string, byte-string, raw-string
     and char literals (newlines stay, so line numbers stay).
  2. Remove every item that a `#[cfg(test)]` or `#[cfg(all(test, ...))]`
     attribute guards: an inline `mod x { ... }` block, a function or impl
     block, or a `mod x;` declaration (then the whole file `x.rs` /
     `x/mod.rs` is skipped).
  3. In the code that is left, count:
       unwrap   `.unwrap()`
       expect   `.expect(`
       macro    `panic!`, `unreachable!`, `todo!`, `unimplemented!`,
                `assert!`, `assert_eq!`, `assert_ne!` (not `debug_assert*`)
       index    `x[...]`: an index or slice expression after an identifier,
                `)` or `]` (not a type, attribute, array literal or pattern)
       slicefn  `copy_from_slice(`, `split_at(`, `split_at_mut(`,
                `clone_from_slice(`, `swap_remove(`
  The index count is syntactic: it includes indexes that a local check
  guards. A reviewer classifies those by hand.

Usage: count_panics.py [--list KIND] PATH...
"""
import os
import re
import sys

KEYWORDS = {
    "as", "in", "mut", "return", "break", "else", "match", "if", "while",
    "for", "let", "const", "static", "dyn", "impl", "where", "move", "ref",
    "yield", "box", "unsafe", "type", "fn", "pub", "use", "loop", "continue",
}


def strip(text):
    """Blank comments and literals; keep newlines and code."""
    out = []
    i, n = 0, len(text)
    while i < n:
        c = text[i]
        nxt = text[i + 1] if i + 1 < n else ""
        if c == "/" and nxt == "/":
            j = text.find("\n", i)
            j = n if j < 0 else j
            out.append(" " * (j - i))
            i = j
        elif c == "/" and nxt == "*":
            depth, j = 1, i + 2
            while j < n and depth:
                if text.startswith("/*", j):
                    depth, j = depth + 1, j + 2
                elif text.startswith("*/", j):
                    depth, j = depth - 1, j + 2
                else:
                    j += 1
            out.append(re.sub(r"[^\n]", " ", text[i:j]))
            i = j
        elif c in "rb" and re.match(r"(br|r)#*\"", text[i:i + 260]) and (
            i == 0 or not (text[i - 1].isalnum() or text[i - 1] == "_")
        ):
            m = re.match(r"(br|r)(#*)\"", text[i:])
            hashes = m.group(2)
            start = i + len(m.group(0))
            end = text.find('"' + hashes, start)
            end = n if end < 0 else end + 1 + len(hashes)
            out.append('""' + re.sub(r"[^\n]", " ", text[i + 2:end]))
            i = end
        elif c == '"' or (c == "b" and nxt == '"' and (
            i == 0 or not (text[i - 1].isalnum() or text[i - 1] == "_")
        )):
            j = i + (2 if c == "b" else 1)
            while j < n and text[j] != '"':
                j += 2 if text[j] == "\\" else 1
            j += 1
            out.append('""' + re.sub(r"[^\n]", " ", text[i + 2:j]))
            i = j
        elif c == "'" or (c == "b" and nxt == "'"):
            k = i + (2 if c == "b" else 1)
            m = re.match(r"(\\(x[0-9a-fA-F]{2}|u\{[0-9a-fA-F]+\}|.)|[^\\'])'", text[k:k + 12])
            if m:
                end = k + len(m.group(0))
                out.append("' '" + " " * (end - i - 3))
                i = end
            else:
                out.append(c)
                i += 1
        else:
            out.append(c)
            i += 1
    return "".join(out)


CFG_TEST = re.compile(r"#\s*\[\s*cfg\s*\(\s*(test|all\s*\(\s*test\b[^\]]*)\)\s*\]")


def match_close(code, open_idx):
    depth = 0
    for j in range(open_idx, len(code)):
        if code[j] == "{":
            depth += 1
        elif code[j] == "}":
            depth -= 1
            if depth == 0:
                return j
    return len(code) - 1


def remove_test_items(code):
    """Return (code without cfg(test) items, [names of cfg(test) `mod x;`])."""
    skipped_mods = []
    while True:
        m = CFG_TEST.search(code)
        if not m:
            return code, skipped_mods
        j = m.end()
        # Skip further attributes.
        while True:
            a = re.match(r"\s*#\s*\[", code[j:])
            if not a:
                break
            depth, k = 0, j + a.end() - 1
            while k < len(code):
                if code[k] == "[":
                    depth += 1
                elif code[k] == "]":
                    depth -= 1
                    if depth == 0:
                        break
                k += 1
            j = k + 1
        semi = code.find(";", j)
        brace = code.find("{", j)
        decl = re.match(r"\s*(pub(\([^)]*\))?\s+)?mod\s+(\w+)\s*;", code[j:])
        if decl:
            skipped_mods.append(decl.group(3))
            end = j + decl.end()
        elif brace >= 0 and (semi < 0 or brace < semi):
            end = match_close(code, brace) + 1
        else:
            end = semi + 1
        blank = re.sub(r"[^\n]", " ", code[m.start():end])
        code = code[:m.start()] + blank + code[end:]


PATTERNS = {
    "unwrap": re.compile(r"\.\s*unwrap\s*\(\s*\)"),
    "expect": re.compile(r"\.\s*expect\s*\("),
    "macro": re.compile(r"(?<![\w])(panic|unreachable|todo|unimplemented|assert|assert_eq|assert_ne)\s*!"),
    "slicefn": re.compile(r"\.\s*(copy_from_slice|clone_from_slice|split_at|split_at_mut|swap_remove)\s*\("),
}
INDEX = re.compile(r"([A-Za-z_][A-Za-z0-9_]*|\)|\])\s*\[")


def count_index(code):
    hits = []
    for m in INDEX.finditer(code):
        tok = m.group(1)
        if tok in KEYWORDS:
            continue
        # `#[attr]` and `#![attr]` are preceded by `#`; `vec![` by `!`; both
        # excluded by the regex itself (the token must be ident, `)` or `]`).
        # Exclude `]` that closes an attribute: `#[derive(..)]\n[`.
        start = m.start()
        if tok == "]":
            line_start = code.rfind("\n", 0, start) + 1
            if code[line_start:start + 1].lstrip().startswith("#"):
                continue
        hits.append(m.end() - 1)
    return hits


def files_in(paths):
    for p in paths:
        if os.path.isfile(p):
            yield p
        else:
            for root, _, names in os.walk(p):
                for name in sorted(names):
                    if name.endswith(".rs"):
                        yield os.path.join(root, name)


def main(argv):
    listing = None
    if argv[:1] == ["--list"]:
        listing, argv = argv[1], argv[2:]
    files = sorted(set(files_in(argv)))
    texts = {}
    skip = set()
    for f in files:
        code, mods = remove_test_items(strip(open(f, encoding="utf-8").read()))
        texts[f] = code
        base = os.path.dirname(f)
        stem = os.path.splitext(os.path.basename(f))[0]
        for mod in mods:
            for cand in (
                os.path.join(base, mod + ".rs"),
                os.path.join(base, mod, "mod.rs"),
                os.path.join(base, stem, mod + ".rs"),
                os.path.join(base, stem, mod, "mod.rs"),
            ):
                skip.add(os.path.normpath(cand))
    kinds = ["unwrap", "expect", "macro", "index", "slicefn"]
    totals = dict.fromkeys(kinds, 0)
    print("%-62s %6s %6s %6s %6s %7s" % ("file", *kinds))
    for f in files:
        if os.path.normpath(f) in skip:
            continue
        code = texts[f]
        row = {}
        found = {}
        for kind in kinds:
            if kind == "index":
                offs = count_index(code)
            else:
                offs = [m.start() for m in PATTERNS[kind].finditer(code)]
            row[kind] = len(offs)
            found[kind] = offs
            totals[kind] += len(offs)
        if any(row.values()):
            print("%-62s %6d %6d %6d %6d %7d" % (f, *[row[k] for k in kinds]))
        if listing:
            lines = open(f, encoding="utf-8").read().split("\n")
            for off in found.get(listing, []):
                ln = code.count("\n", 0, off)
                print("    %s:%d: %s" % (f, ln + 1, lines[ln].strip()))
    print("%-62s %6d %6d %6d %6d %7d" % ("TOTAL", *[totals[k] for k in kinds]))
    print("skipped cfg(test) files:", ", ".join(sorted(s for s in skip if os.path.exists(s))) or "none")


if __name__ == "__main__":
    main(sys.argv[1:])
