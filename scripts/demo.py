#!/usr/bin/env python3
"""Records the terminal session shown at the top of the README as an animated SVG.

Nothing in it is typed by hand: the two documents are made with `pdfops create`, every
command is run by a shell, and what it printed is what the recording shows.

    scripts/demo.py --pdfops target/release/pdfops --out .github/demo.svg

Needs jq, which one of the commands pipes into, and Node: the recording is drawn by
`npx svg-term-cli`.
"""

import argparse
import json
import os
import shutil
import subprocess
import sys
import tempfile

WIDTH = 96           # columns of the picture
COLUMNS = 92         # columns of the terminal in it; narrower, since viewers differ in font width
PROMPT = "\x1b[1;32m$\x1b[0m "
KEY = 0.035          # seconds per typed character
PAUSE = 1.6          # reading time after each output
SVG_TERM = "svg-term-cli@2.1.1"

INVOICE = """# Invoice 2026/041

Billed to Northwind Ltd, due 2026-11-05.

| Item | Qty | Unit price | Total |
| --- | ---: | ---: | ---: |
| Design review | 6 | 120.00 | 720.00 |
| Implementation | 31 | 95.00 | 2945.00 |
| Hosting, October | 1 | 49.00 | 49.00 |
"""

CONTRACT = """# Service agreement

This agreement is made between Northwind Ltd and Jan Kowalski, the contractor.

Jan Kowalski will deliver the work described in annex A by 2026-12-01.

Payments go to the account named by Jan Kowalski in writing.
"""

# What is typed, and the note shown after it. Each runs in the scratch directory.
STEPS = [
    ("pdfops tables invoice.pdf --format markdown | jq -r '.tables[].markdown'", "as a table"),
    ('pdfops redact contract.pdf --text "Jan Kowalski" -o safe.pdf', "removed, then verified"),
    ('pdfops search safe.pdf "Kowalski"', "and it is gone"),
]


def wrapped(text):
    """The text broken where a terminal of COLUMNS would break it."""
    return "\n".join(line[at:at + COLUMNS] for line in text.split("\n")
                     for at in range(0, max(len(line), 1), COLUMNS))


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--pdfops", default="pdfops")
    parser.add_argument("--out", default=".github/demo.svg")
    parser.add_argument("--cast", help="also keep the asciicast the SVG is drawn from")
    args = parser.parse_args()

    pdfops = shutil.which(args.pdfops) or sys.exit(f"{args.pdfops} not found")
    pdfops = os.path.abspath(pdfops)
    shutil.which("jq") or sys.exit("jq not found")
    # The commands say `pdfops`: make that the binary under test.
    environment = dict(os.environ, PATH=os.path.dirname(pdfops) + os.pathsep + os.environ["PATH"])
    events, clock, rows = [], 0.5, 0

    def emit(text, wait=0.0):
        nonlocal clock, rows
        clock += wait
        events.append([round(clock, 3), "o", text.replace("\n", "\r\n")])
        rows += text.count("\n")

    with tempfile.TemporaryDirectory() as scratch:
        for name, source in (("invoice", INVOICE), ("contract", CONTRACT)):
            with open(os.path.join(scratch, name + ".md"), "w", encoding="utf-8") as handle:
                handle.write(source)
            subprocess.run([pdfops, "create", name + ".md", "-o", name + ".pdf"], cwd=scratch, check=True,
                           stdout=subprocess.DEVNULL)

        for typed, note in STEPS:
            line = f"$ {typed}   # {note}"
            len(line) <= COLUMNS or sys.exit(f"too long for one line: {line}")
            emit(PROMPT)
            for character in typed:
                emit(character, KEY)
            emit(f"   \x1b[2m# {note}\x1b[0m", 0.3)
            emit("\n", 0.5)
            run = subprocess.run(typed, shell=True, cwd=scratch, env=environment, capture_output=True, text=True)
            if run.returncode != 0:
                sys.exit(f"{typed}: {run.stderr.strip()}")
            emit(wrapped(run.stdout.strip()) + "\n\n", 0.25)
            clock += PAUSE
        emit(PROMPT)
        clock += 2.5
        emit("")

        header = {"version": 2, "width": WIDTH, "height": rows + 1}
        cast = args.cast or os.path.join(scratch, "demo.cast")
        with open(cast, "w", encoding="utf-8") as handle:
            handle.write("\n".join(json.dumps(line) for line in [header] + events) + "\n")
        os.makedirs(os.path.dirname(os.path.abspath(args.out)), exist_ok=True)
        subprocess.run(["npx", "-y", SVG_TERM, "--in", cast, "--out", os.path.abspath(args.out), "--window",
                        "--width", str(WIDTH), "--height", str(rows + 1)], check=True)
    print(f"{args.out}: {rows + 1} rows, {clock:.1f} s")


if __name__ == "__main__":
    main()
