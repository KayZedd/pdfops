#!/usr/bin/env python3
"""Times every pdfops command against the usual alternatives and prints a Markdown table.

Each cell is the best wall-clock time of several whole-process runs, start-up
included, because that is what one tool call costs an agent.

    scripts/bench.py --pdfops target/release/pdfops --doc manual.pdf \\
        --images-doc pictures.pdf --form-doc form.pdf --python venv/bin/python

--python is an interpreter with pypdf, pdfplumber and pymupdf installed; without
it those columns are left out. Tools that are not installed show as "n/a".
"""

import argparse
import os
import shutil
import subprocess
import sys
import tempfile
import threading
import time

RUNS = 3
TIMEOUT = 60


def best(command, scratch):
    """Best time in ms, None if the tool is missing or fails, inf on timeout."""
    if command is None or shutil.which(command[0]) is None:
        return None
    times = []
    for _ in range(RUNS):
        shutil.rmtree(scratch, ignore_errors=True)
        os.makedirs(scratch)
        start = time.perf_counter()
        process = subprocess.Popen(command, stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL,
                                   stderr=subprocess.DEVNULL)
        # A watchdog rather than wait(timeout=...), which polls and would round short runs up.
        watchdog = threading.Timer(TIMEOUT, process.kill)
        watchdog.start()
        code = process.wait()
        elapsed = time.perf_counter() - start
        expired = not watchdog.is_alive()
        watchdog.cancel()
        if expired:
            return float("inf")
        times.append(elapsed)
        # qpdf exits with 3 for warnings on output it still wrote.
        if code not in (0, 3):
            return None
    return min(times) * 1000


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--pdfops", default="pdfops")
    parser.add_argument("--doc", required=True, help="a long text document")
    parser.add_argument("--images-doc", help="a document with embedded images")
    parser.add_argument("--form-doc", help="a document with form fields")
    parser.add_argument("--form-field", help="name of a text field in the form document")
    parser.add_argument("--python", help="interpreter with pypdf, pdfplumber and pymupdf")
    parser.add_argument("--word", default="the", help="a word that occurs in the document")
    parser.add_argument("--ocr-lang", default="eng")
    args = parser.parse_args()

    work = tempfile.mkdtemp(prefix="pdfops-bench-")
    T = os.path.join(work, "out")
    B, F, W = args.pdfops, os.path.abspath(args.doc), args.word
    IMG = os.path.abspath(args.images_doc) if args.images_doc else None
    FORM = os.path.abspath(args.form_doc) if args.form_doc else None

    def py(code):
        return [args.python, "-c", code] if args.python else None

    # Inputs that some rows need: an encrypted copy, a one-page scan and a Markdown file.
    locked = os.path.join(work, "locked.pdf")
    subprocess.run([B, "encrypt", F, "--owner-password", "o", "--user-password", "u", "-o", locked],
                   check=True, stdout=subprocess.DEVNULL)
    subprocess.run([B, "render", F, "-p", "30", "--dpi", "200", "-o", work], check=True, stdout=subprocess.DEVNULL)
    page_png = os.path.join(work, "page-0030.png")
    scan = os.path.join(work, "scan.pdf")
    subprocess.run([B, "create", "--markdown", f"![scan]({page_png})", "--margin", "0", "-o", scan],
                   check=True, stdout=subprocess.DEVNULL)
    markdown = os.path.join(work, "doc.md")
    with open(markdown, "w") as out:
        out.write("# Report\n\n" + "".join(
            f"## Section {i}\n\nSome **bold** text, a [link](https://example.com) and `code`.\n\n"
            f"| Item | Qty |\n| --- | --- |\n| Bolt {i} | {i * 3} |\n\n- one\n- two\n\n" for i in range(1, 101)))
    # A throwaway signing identity, and a signed copy of the document to verify.
    key, cert, signed = (os.path.join(work, n) for n in ("key.pem", "cert.pem", "signed.pdf"))
    can_sign = shutil.which("openssl") is not None and subprocess.run(
        ["openssl", "req", "-x509", "-newkey", "rsa:2048", "-nodes", "-keyout", key, "-out", cert,
         "-days", "2", "-subj", "/CN=Benchmark"], capture_output=True).returncode == 0
    if can_sign:
        subprocess.run([B, "sign", F, "--cert", cert, "--key", key, "-o", signed], check=True, stdout=subprocess.DEVNULL)
    pyhanko = os.path.join(os.path.dirname(args.python), "pyhanko") if args.python else "pyhanko"
    tessdata = os.path.expanduser("~/.local/share/pdfops/tessdata")
    tesseract = ["tesseract", page_png, "stdout", "-l", args.ocr_lang]
    if os.path.exists(os.path.join(tessdata, args.ocr_lang + ".traineddata")):
        tesseract += ["--tessdata-dir", tessdata]

    mu = "import pymupdf; d = pymupdf.open(%r); " % F
    pp = "from pypdf import PdfReader, PdfWriter; r = PdfReader(%r); " % F
    pl = "import pdfplumber; d = pdfplumber.open(%r); " % F
    o = os.path.join(T, "o.pdf")

    # (task, pdfops, command line tool, PyMuPDF, pypdf, pdfplumber)
    rows = [
        ("`info`", [B, "info", F], ["pdfinfo", F],
         py(mu + "print(d.page_count, d.metadata)"), py(pp + "print(len(r.pages), r.metadata)"),
         py(pl + "print(len(d.pages), d.metadata)")),
        ("`text`, all pages", [B, "text", F], ["pdftotext", F, os.path.join(T, "o.txt")],
         py(mu + "[p.get_text() for p in d]"), py(pp + "[p.extract_text() for p in r.pages]"),
         py(pl + "[p.extract_text() for p in d.pages]")),
        ("`search`, all pages", [B, "search", F, W], None,
         py(mu + "[p.search_for(%r) for p in d]" % W), None, None),
        ("`layout`, every word with its box", [B, "layout", F, "--level", "words"], None,
         py(mu + "[p.get_text('words') for p in d]"), None, py(pl + "[p.extract_words() for p in d.pages]")),
        ("`tables`, 50 pages", [B, "tables", F, "-p", "1-50"], None,
         py(mu + "[p.find_tables().tables for p in d.pages(0, 50)]"), None,
         py(pl + "[p.extract_tables() for p in d.pages[:50]]")),
        ("`outline`", [B, "outline", F], None, py(mu + "d.get_toc()"), py(pp + "r.outline"), None),
        ("`render`, 20 pages at 150 dpi", [B, "render", F, "-p", "1-20", "-o", T],
         ["pdftoppm", "-r", "150", "-png", "-f", "1", "-l", "20", F, os.path.join(T, "p")],
         py(mu + "[d[i].get_pixmap(dpi=150).save(%r %% i) for i in range(20)]" % os.path.join(T, "p%d.png")),
         None, None),
        ("`ocr`, one page", [B, "ocr", scan, "--lang", args.ocr_lang], tesseract, None, None, None),
        ("`create`, 100 sections of Markdown", [B, "create", markdown, "-o", o], None, None, None, None),
        ("`merge`, three copies", [B, "merge", F, F, F, "-o", o], ["qpdf", "--empty", "--pages", F, F, F, "--", o],
         py(mu + "[d.insert_pdf(pymupdf.open(%r)) for _ in range(2)]; d.save(%r)" % (F, o)),
         py("from pypdf import PdfWriter; w = PdfWriter(); [w.append(%r) for _ in range(3)]; w.write(%r)" % (F, o)), None),
        ("`pages`, keep 10", [B, "pages", F, "-k", "1-10", "-o", o], ["qpdf", F, "--pages", ".", "1-10", "--", o],
         py(mu + "d.select(range(10)); d.save(%r, garbage=3)" % o),
         py(pp + "w = PdfWriter(); [w.add_page(r.pages[i]) for i in range(10)]; w.write(%r)" % o), None),
        ("`split`, one file per page", [B, "split", F, "-o", T], ["pdfseparate", F, os.path.join(T, "p-%d.pdf")],
         py(mu + "\nfor i in range(d.page_count):\n n = pymupdf.open(); n.insert_pdf(d, from_page=i, to_page=i); n.save(%r %% i)"
            % os.path.join(T, "p%d.pdf")),
         py(pp + "\nfor i, p in enumerate(r.pages):\n w = PdfWriter(); w.add_page(p); w.write(%r %% i)"
            % os.path.join(T, "p%d.pdf")), None),
        ("`rotate`, all pages", [B, "rotate", F, "-a", "90", "-o", o], ["qpdf", F, "--rotate=+90", o],
         py(mu + "[p.set_rotation(90) for p in d]; d.save(%r)" % o),
         py(pp + "w = PdfWriter(clone_from=r); [p.rotate(90) for p in w.pages]; w.write(%r)" % o), None),
        ("`stamp`, text on every page", [B, "stamp", F, "-t", "CONFIDENTIAL", "-o", o], None,
         py(mu + "[p.insert_text((72, 72), 'CONFIDENTIAL', fontsize=40) for p in d]; d.save(%r)" % o), None, None),
        ("`stamp`, QR code on every page", [B, "stamp", F, "--qr", "https://example.com/doc/42", "-o", o],
         None, None, None, None),
        ("`annotations`, list", [B, "annotations", F], None,
         py(mu + "[[a.type for a in p.annots()] + p.get_links() for p in d]"), None,
         py(pl + "[p.annots for p in d.pages]")),
        ("`annotate`, highlight a word on every page", [B, "annotate", F, "--text", W, "-o", o], None,
         py(mu + "\nfor p in d:\n [p.add_highlight_annot(q) for q in p.search_for(%r)]\nd.save(%r)" % (W, o)),
         None, None),
        ("`redact`, a word on every page", [B, "redact", F, "--text", W, "-o", o], None,
         py(mu + "\nfor p in d:\n [p.add_redact_annot(q) for q in p.search_for(%r)]; p.apply_redactions()\nd.save(%r)" % (W, o)),
         None, None),
        ("`replace`, a word on every page", [B, "replace", F, "--find", W, "--with", "XX", "-o", o],
         None, None, None, None),
        ("`set-meta`", [B, "set-meta", F, "--title", "New title", "-o", o], None,
         py(mu + "d.set_metadata({'title': 'New title'}); d.save(%r)" % o),
         py(pp + "w = PdfWriter(clone_from=r); w.add_metadata({'/Title': 'New title'}); w.write(%r)" % o), None),
        ("`compress`", [B, "compress", F, "-o", o],
         ["qpdf", F, "--object-streams=generate", "--recompress-flate", "--compression-level=9", o],
         py(mu + "d.save(%r, garbage=3, deflate=True)" % o), None, None),
        ("`encrypt`, AES-256", [B, "encrypt", F, "--owner-password", "o", "--user-password", "u", "-o", o],
         ["qpdf", F, "--encrypt", "u", "o", "256", "--", o],
         py(mu + "d.save(%r, encryption=pymupdf.PDF_ENCRYPT_AES_256, owner_pw='o', user_pw='u')" % o),
         py(pp + "w = PdfWriter(clone_from=r); w.encrypt('u', 'o', algorithm='AES-256'); w.write(%r)" % o), None),
        ("`decrypt`", [B, "decrypt", locked, "--password", "u", "-o", o], ["qpdf", "--password=u", "--decrypt", locked, o],
         py("import pymupdf; d = pymupdf.open(%r); d.authenticate('u'); d.save(%r)" % (locked, o)),
         py("from pypdf import PdfReader, PdfWriter; r = PdfReader(%r); r.decrypt('u'); "
            "w = PdfWriter(clone_from=r); w.write(%r)" % (locked, o)), None),
    ]
    if can_sign:
        rows += [
            ("`sign`, RSA-2048", [B, "sign", F, "--cert", cert, "--key", key, "-o", o],
             [pyhanko, "sign", "addsig", "--field", "Sig1", "pemder", "--key", key, "--cert", cert, "--no-pass", F, o],
             None, None, None),
            ("`signatures`, verify", [B, "signatures", signed], ["pdfsig", signed], None, None, None),
        ]
    if IMG:
        rows.insert(8, ("`images`, all embedded images", [B, "images", IMG, "-o", T],
                        ["pdfimages", "-png", IMG, os.path.join(T, "i")],
                        py("import pymupdf; d = pymupdf.open(%r); "
                           "[open(%r %% x[0], 'wb').write(d.extract_image(x[0])['image']) "
                           "for p in d for x in p.get_images()]" % (IMG, os.path.join(T, "i%d.bin"))),
                        py("from pypdf import PdfReader; r = PdfReader(%r); "
                           "[open(%r %% (n, i), 'wb').write(im.data) for n, p in enumerate(r.pages) "
                           "for i, im in enumerate(p.images)]" % (IMG, os.path.join(T, "i%d-%d.bin"))), None))
    if FORM:
        field = args.form_field or "name"
        rows += [
            ("`forms`, list fields", [B, "forms", FORM], None,
             py("import pymupdf; d = pymupdf.open(%r); print([(w.field_name, w.field_value) for p in d for w in p.widgets()])" % FORM),
             py("from pypdf import PdfReader; print(PdfReader(%r).get_fields())" % FORM), None),
            ("`fill`, one field", [B, "fill", FORM, "--set", field + "=Ada", "-o", o], None,
             py("import pymupdf; d = pymupdf.open(%r)\nfor p in d:\n for w in p.widgets():\n"
                "  if w.field_name == %r:\n   w.field_value = 'Ada'; w.update()\nd.save(%r)" % (FORM, field, o)),
             py("from pypdf import PdfReader, PdfWriter; w = PdfWriter(clone_from=PdfReader(%r)); "
                "w.update_page_form_field_values(w.pages[0], {%r: 'Ada'}, auto_regenerate=False); w.write(%r)"
                % (FORM, field, o)), None),
        ]

    names = ["pdfops", "command line tool", "PyMuPDF", "pypdf", "pdfplumber"]
    keep = [0, 1] + ([2, 3, 4] if args.python else [])
    print("| Task | " + " | ".join(names[i] for i in keep) + " |")
    print("| --- |" + " ---: |" * len(keep))
    for task, *commands in rows:
        times = [best(c, T) for c in commands]
        fastest = min((t for t in times if t is not None), default=None)
        cells = []
        for i in keep:
            t, command = times[i], commands[i]
            if command is None:
                cell = "-"
            elif t is None:
                cell = "n/a"
            elif t == float("inf"):
                cell = f"over {TIMEOUT} s"
            else:
                cell = f"{t:.0f} ms" if t < 10000 else f"{t / 1000:.1f} s"
                if t == fastest:
                    cell = f"**{cell}**"
            if i == 1 and command is not None:
                cell = f"`{os.path.basename(command[0])}` {cell}"
            cells.append(cell)
        print(f"| {task} | " + " | ".join(cells) + " |", flush=True)
    shutil.rmtree(work, ignore_errors=True)


if __name__ == "__main__":
    sys.exit(main())
