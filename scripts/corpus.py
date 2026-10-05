#!/usr/bin/env python3
"""Runs every pdfops command over a directory of PDFs and reports what went wrong.

A command may fail on a broken file, as long as it says so cleanly. What counts
as a defect:

  crash    it died (also by exceeding the memory cap), or reported an internal error
  timeout  it did not finish
  invalid  it wrote a PDF that qpdf rejects although the input was sound

    scripts/corpus.py --pdfops target/release/pdfops path/to/pdfs

Exits with status 1 if any defect was found.
"""

import argparse
import collections
import concurrent.futures
import json
import os
import shutil
import subprocess
import sys
import tempfile

TIMEOUT = 60
# Address space each process may use. A corpus contains files built to exhaust
# memory; without a cap one of them takes the whole machine down.
MEMORY_LIMIT = 2 * 1024**3


# Certificate and key for the `sign` command, made once per run when openssl is at hand.
IDENTITY = None


def limit_memory():
    import resource

    resource.setrlimit(resource.RLIMIT_AS, (MEMORY_LIMIT, MEMORY_LIMIT))


def run(command, timeout=TIMEOUT):
    try:
        done = subprocess.run(command, stdin=subprocess.DEVNULL, capture_output=True, timeout=timeout,
                              preexec_fn=limit_memory if os.name == "posix" else None)
    except subprocess.TimeoutExpired:
        return None, b""
    return done.returncode, done.stderr


def sound(path):
    """Whether qpdf reads the file without errors (warnings are fine)."""
    code, _ = run(["qpdf", "--check", path], 120)
    return code in (0, 3)


def examine(pdfops, path, scratch):
    work = tempfile.mkdtemp(dir=scratch)
    out = os.path.join(work, "out.pdf")
    commands = {
        "info": ["info", path],
        "text": ["text", path, "--max-chars", "20000"],
        "search": ["search", path, "the"],
        "layout": ["layout", path, "-p", "1"],
        "tables": ["tables", path, "-p", "1"],
        "outline": ["outline", path],
        "annotations": ["annotations", path],
        "signatures": ["signatures", path],
        "forms": ["forms", path],
        "scan": ["scan", path],
        "sanitize": ["sanitize", path, "-o", out],
        "render": ["render", path, "-p", "1", "--dpi", "50", "-o", work],
        "images": ["images", path, "-p", "1", "-o", work],
        "pages": ["pages", path, "-k", "1", "-o", out],
        "merge": ["merge", path, path, "-o", out],
        "split": ["split", path, "--every", "5000", "-o", os.path.join(work, "parts")],
        "rotate": ["rotate", path, "-a", "90", "-o", out],
        "stamp": ["stamp", path, "-t", "DRAFT {page}/{pages}", "-o", out],
        "set-meta": ["set-meta", path, "--title", "Corpus", "-o", out],
        "compress": ["compress", path, "-o", out],
        "encrypt": ["encrypt", path, "--owner-password", "o", "-o", out],
        "annotate": ["annotate", path, "--rect", "1:50,50,300,300", "-o", out],
        "redact": ["redact", path, "--rect", "1:50,50,300,300", "-o", out],
        "replace": ["replace", path, "--find", "the", "--with", "THE", "-p", "1", "-o", out],
    }
    if IDENTITY:
        commands["sign"] = ["sign", path, "--cert", IDENTITY[0], "--key", IDENTITY[1], "--visible", "1:50,50,250,110",
                            "-o", out]
    writes = {"pages", "merge", "rotate", "stamp", "set-meta", "compress", "encrypt", "annotate", "redact",
              "replace", "sign", "sanitize"}
    input_sound = None
    findings = []
    for name, args in commands.items():
        if os.path.exists(out):
            os.remove(out)
        # pdfops' own limits sit below the runner's, so a hostile file shows up as a
        # refusal with a reason, not as a kill.
        code, stderr = run([pdfops, "--max-memory", "1024", "--timeout", "30"] + args)
        message = stderr.decode("utf-8", "replace").strip()[:200]
        if code is None:
            findings.append((name, "timeout", ""))
        elif code not in (0, 1) or "internal error" in message:
            died = f"killed by signal {-code}" if code < 0 else f"exit status {code}"
            findings.append((name, "crash", f"{died} {message}".strip()))
        elif code == 0 and name in writes and os.path.exists(out):
            if not sound(out):
                if input_sound is None:
                    input_sound = sound(path)
                if input_sound:
                    _, why = run(["qpdf", "--check", out], 120)
                    findings.append((name, "invalid", why.decode("utf-8", "replace").strip()[:200]))
            outcome = "ok"
        else:
            outcome = "ok" if code == 0 else "refused"
        if not findings or findings[-1][0] != name:
            findings.append((name, "ok" if code == 0 else "refused", message if code else ""))
    shutil.rmtree(work, ignore_errors=True)
    return os.path.basename(path), findings


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("directory")
    parser.add_argument("--pdfops", default="pdfops")
    parser.add_argument("--jobs", type=int, default=2)
    parser.add_argument("--limit", type=int, help="only the first N files")
    parser.add_argument("--report", help="write every defect to this JSON file")
    args = parser.parse_args()

    files = sorted(os.path.join(args.directory, f) for f in os.listdir(args.directory) if f.lower().endswith(".pdf"))
    files = files[: args.limit] if args.limit else files
    scratch = tempfile.mkdtemp(prefix="pdfops-corpus-")
    global IDENTITY
    cert, key = os.path.join(scratch, "cert.pem"), os.path.join(scratch, "key.pem")
    made = shutil.which("openssl") and subprocess.run(
        ["openssl", "req", "-x509", "-newkey", "rsa:2048", "-nodes", "-keyout", key, "-out", cert,
         "-days", "2", "-subj", "/CN=Corpus"], capture_output=True).returncode == 0
    IDENTITY = (cert, key) if made else None
    tally = collections.defaultdict(collections.Counter)
    defects = []
    with concurrent.futures.ThreadPoolExecutor(args.jobs) as pool:
        for name, findings in pool.map(lambda f: examine(args.pdfops, f, scratch), files):
            for command, outcome, message in findings:
                tally[command][outcome] += 1
                if outcome in ("crash", "timeout", "invalid"):
                    defects.append({"file": name, "command": command, "kind": outcome, "message": message})
    shutil.rmtree(scratch, ignore_errors=True)

    kinds = ["ok", "refused", "crash", "timeout", "invalid"]
    print(f"{len(files)} files")
    print("command".ljust(12) + "".join(k.rjust(9) for k in kinds))
    for command, counts in tally.items():
        print(command.ljust(12) + "".join(str(counts[k]).rjust(9) for k in kinds))
    for defect in defects[:60]:
        print(f"{defect['kind'].upper()} {defect['command']} {defect['file']}: {defect['message']}")
    if args.report:
        with open(args.report, "w") as report:
            json.dump(defects, report, indent=1)
    return 1 if defects else 0


if __name__ == "__main__":
    sys.exit(main())
