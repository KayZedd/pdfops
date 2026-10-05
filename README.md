<h1 align="center">pdfops</h1>

<p align="center">
  <b>Fast PDF tools for AI agents.</b><br>
  One binary, 31 tools, JSON in and out. Works as a CLI, an MCP server and a Rust library.
</p>

<p align="center">
  <a href="https://github.com/KayZedd/pdfops/actions/workflows/ci.yml"><img src="https://github.com/KayZedd/pdfops/actions/workflows/ci.yml/badge.svg" alt="CI"></a>
  <a href="https://crates.io/crates/pdfops"><img src="https://img.shields.io/crates/v/pdfops.svg" alt="crates.io"></a>
  <a href="https://www.npmjs.com/package/pdfops-cli"><img src="https://img.shields.io/npm/v/pdfops-cli.svg" alt="npm"></a>
  <a href="https://github.com/KayZedd/pdfops/releases/latest"><img src="https://img.shields.io/github/v/release/KayZedd/pdfops.svg" alt="release"></a>
  <a href="LICENSE"><img src="https://img.shields.io/badge/license-MIT-blue.svg" alt="MIT license"></a>
</p>

```sh
$ pdfops tables invoice.pdf --format markdown        # tables as tables, not as a blob of text
$ pdfops redact contract.pdf --text "Jan Kowalski" -o safe.pdf   # removed from the file, then verified
$ pdfops text scan.pdf --ocr --ocr-lang pol+eng      # OCR only where there is no text layer
$ pdfops stamp offer.pdf --image signature.png --x 380 --y 690 -o signed.pdf
```

## Why pdfops

- **Built for agents.** Every command returns one JSON document, errors say what to do next, and
  text can be read under a character budget with a resume point.
- **Everything in one place.** Reading, layout, tables, OCR, rendering, page surgery, stamping,
  redaction, annotations, signatures, forms, encryption, inspection and creation: 31 tools behind
  one schema.
- **Nothing to set up.** A single binary with no PDF libraries, no Python and no runtime. Only OCR
  needs an extra program, and pdfops can fetch the language data itself.
- **Fast.** Files open in milliseconds whatever their size, and page work runs on all cores. See the
  [benchmarks](#benchmarks).
- **Redaction you can trust.** Content is deleted from the page, not covered, and the result is
  checked by a second interpreter before anything is written.
- **Safe on files you did not write.** Every command runs under a memory cap and a time limit, the
  MCP server runs each call in a process of its own and can be confined to one directory, and
  every command is exercised against a corpus of 988 hostile and malformed PDFs in CI.
- **Knows what a file is up to.** `scan` reports scripts, actions that fire by themselves, attached
  programs, disguised names and text that is extracted but cannot be seen, the carrier of prompt
  injection. `sanitize` removes the active content and proves it by scanning the result.

## Quick start

As an MCP server, with nothing installed beforehand:

```json
{
  "mcpServers": {
    "pdfops": { "command": "npx", "args": ["-y", "pdfops-cli", "mcp"] }
  }
}
```

For Claude Code: `claude mcp add pdfops -- npx -y pdfops-cli mcp`.

Tools are named `pdf_info`, `pdf_text`, `pdf_redact` and so on, and take the same arguments as the
CLI. Relative paths resolve against the server's working directory.

To keep an agent inside one folder, add `--root`: every path outside it is refused, including paths
that arrive inside a document, and relative paths resolve against it.

```json
{ "command": "npx", "args": ["-y", "pdfops-cli", "mcp", "--root", "/home/me/documents"] }
```

### Install

| Method | Command | Needs |
| --- | --- | --- |
| npm | `npm install -g pdfops-cli` or `npx pdfops-cli` | Node 18+ |
| Prebuilt, via cargo | `cargo binstall pdfops` | [cargo-binstall](https://github.com/cargo-bins/cargo-binstall) |
| From source | `cargo install pdfops` | Rust 1.92+ |
| Docker | `docker run --rm -i -v "$PWD:/work" ghcr.io/kayzedd/pdfops mcp --root /work` | Docker |
| Manual | [download an archive](https://github.com/KayZedd/pdfops/releases/latest) and put `pdfops` on your `PATH` | nothing |

The npm package is called `pdfops-cli` and installs the `pdfops` command. It is a small launcher:
on first run it downloads the binary for your platform from the GitHub release, checks it against
the published SHA-256 sum and caches it. Prebuilt binaries cover Linux (glibc and musl) and macOS
on x86-64 and ARM64, and Windows on x86-64. The Docker image is Alpine with pdfops and tesseract
with English; add `-v` for the folder to work in.

OCR additionally needs the `tesseract` program; nothing else does. Language data is fetched on
request, without administrator rights:

```sh
pdfops ocr-langs                     # is tesseract there, which languages can be used
pdfops ocr-install --lang pol+eng    # download language data into the user's data directory
pdfops ocr-install --engine          # install tesseract itself where that needs no password
```

## Tools

| | Command | What it does |
| --- | --- | --- |
| **Read** | `info` | Page count, page size, metadata, encryption, outline and form summary |
| | `text` | Text page by page, with a character budget and optional OCR fallback |
| | `search` | Find text or a regex, returns pages and snippets |
| | `layout` | Bounding box, font and size of every line or word |
| | `tables` | Tables as rows of cells, Markdown or CSV |
| | `outline` | Bookmarks with target pages |
| | `annotations` | Highlights, comments, links and other markup, with positions |
| | `signatures` | Digital signatures: who signed, and whether the document changed since |
| | `render` | Pages to PNG, to look at charts, scans and layout |
| | `images` | The images drawn on pages, as files |
| | `ocr` | Recognised text of scanned pages, optionally written into a searchable copy |
| | `scan` | Scripts, automatic actions, attachments, disguised content and hidden text, by severity |
| **Build** | `create` | A new PDF from Markdown |
| | `merge` | Several PDFs into one |
| | `pages` | Keep, reorder, duplicate or delete pages |
| | `split` | Split by page count or by ranges |
| **Edit** | `rotate` | Rotate pages by multiples of 90 degrees |
| | `stamp` | Watermark, header, footer, page numbers, an image such as a signature, or a QR code |
| | `annotate` | Add a highlight, underline, strike-out, box, note or link |
| | `replace` | Replace text in place, in the document's own font where possible |
| | `redact` | Remove text, images and drawings in areas or matching text, then verify |
| | `set-meta` | Title, author, subject, keywords, creator |
| | `compress` | Shrink: lossless by default, optionally re-encoding and downscaling images |
| **Forms** | `forms` | Fields with their types, values and options |
| | `fill` | Fill fields by name |
| **Protect** | `encrypt` | AES-256 passwords and permissions |
| | `decrypt` | Remove password protection |
| | `sign` | Sign digitally with a certificate, keeping earlier signatures valid |
| | `sanitize` | Remove scripts, risky actions, attachments, XFA and media, then verify by scanning |
| **Setup** | `ocr-langs` | Whether tesseract is installed and which languages are usable |
| | `ocr-install` | Download OCR language data, optionally install tesseract |

Run `pdfops <command> --help` for the options of each.

## Examples

```sh
$ pdfops info manual.pdf
{"encrypted":false,"file":"manual.pdf","form_fields":0,
 "metadata":{"created":"2026-06-30T09:07:46+00:00","producer":"GPL Ghostscript 10.07.1"},
 "outline_entries":645,"page_size_pt":{"height":792.0,"width":595.0},"pages":357,"pdf_version":"1.3",
 "size_bytes":1386723,"uniform_page_size":true}

$ pdfops search manual.pdf "calling convention" --max-results 1 --context 40
{"file":"manual.pdf","matches":[{"match":"Calling Convention","page":12,
 "snippet":"... 10.5.1 The Pascal Calling Convention ..."}],"query":"calling convention",
 "total_matches":17,"unreadable_pages":[]}
```

Reading:

```sh
pdfops text manual.pdf --pages 12- --max-chars 4000     # resume_at_page says where to continue
pdfops text scan.pdf --ocr --ocr-lang pol+eng           # OCR only the pages that have no text
pdfops tables report.pdf --pages 4 --format markdown
pdfops layout report.pdf --pages 4 --level words        # bbox, font and size per word
pdfops render report.pdf --pages 1-3 --dpi 150 -o out/  # look at charts and layout
pdfops --stream ocr scan.pdf --lang pol                 # a line per page as it finishes
pdfops ocr scan.pdf --lang pol -o searchable.pdf        # the same file, with text to search
pdfops images report.pdf -o images/
```

Building and page work:

```sh
pdfops create notes.md -o notes.pdf
pdfops merge a.pdf b.pdf -o merged.pdf
pdfops pages in.pdf --keep "3,1,5-" -o out.pdf
pdfops split in.pdf --every 10 -o parts/
```

Editing:

```sh
pdfops stamp in.pdf --text "Poufne · {page}/{pages}" --position footer -o out.pdf
pdfops stamp in.pdf --image signature.png --x 380 --y 690 --width 140 --pages last -o out.pdf
pdfops stamp in.pdf --qr "https://example.com/doc/42" --anchor bottom-right -o out.pdf
pdfops redact in.pdf --text "Jan Kowalski" --text "\d{11}" --regex -o redacted.pdf
pdfops redact in.pdf --rect "2:100,200,300,220" -o redacted.pdf
pdfops replace in.pdf --find "2025" --with "2026" -o out.pdf
pdfops replace in.pdf --find "2025" --with "2026" --dry-run -o out.pdf   # the plan, nothing written
pdfops compress in.pdf --max-image-edge 1600 --image-quality 70 -o small.pdf
```

Forms and protection:

```sh
pdfops forms form.pdf
pdfops fill form.pdf --set name="Ada Lovelace" --set agree=true -o filled.pdf
pdfops encrypt in.pdf --owner-password secret --deny-copy -o locked.pdf
pdfops sign in.pdf --p12 identity.p12 --p12-password secret --reason "Approved" -o signed.pdf
pdfops sign in.pdf --cert me.crt --key me.key --visible "1:360,700,560,760" -o signed.pdf
pdfops signatures signed.pdf                            # valid, unchanged, who and when
pdfops signatures signed.pdf --trust company-root.pem   # and whether the signer is one of yours
pdfops scan inbox/offer.pdf                             # what is in it, before reading it
pdfops sanitize inbox/offer.pdf -o offer-clean.pdf      # scripts, actions, attachments removed
```

Markup:

```sh
pdfops annotate in.pdf --text "liability" --comment "check with legal" -o marked.pdf
pdfops annotate in.pdf --kind link --rect "1:72,50,300,70" --url https://example.com -o out.pdf
pdfops annotations marked.pdf
```

### Conventions

- **Output.** Every command prints one JSON document on stdout. Errors go to stderr as
  `{"error": "..."}` with exit status 1. Add `--pretty` to indent.
- **Progress.** With `--stream`, `ocr`, `text --ocr`, `render`, `images`, `split`, `redact` and
  `replace` print one line of JSON per finished page or file,
  `{"event":"progress","step":"render","done":7,"total":20,"page":12,...}`, and the usual result as
  the last line. Pages are worked on in parallel, so events come in the order the work finishes;
  each names its page and `done` rises by one per line. Over MCP the same events arrive as
  `notifications/progress` when the call carries a progress token.
- **Writing.** Commands that write take `-o`. It may be the input file: output goes through a
  temporary file.
- **Pages** are 1-based and comma separated: `3`, `2-5`, `7-` (to the end), `-4` (from the start),
  `5-2` (descending), `last`, `odd`, `even`, `all`.
- **Positions** are in points with the origin at the top-left corner of the page as displayed, y
  growing downwards. `layout` reports them, `stamp --x/--y` and `redact --rect` accept them, and a
  pixel of `render --dpi 72` is exactly one point.

## How it compares

| | pdfops | PyMuPDF | pypdf | pdfplumber | qpdf | poppler-utils |
| --- | :---: | :---: | :---: | :---: | :---: | :---: |
| Text extraction | ✓ | ✓ | ✓ | ✓ | – | ✓ |
| Word positions and fonts | ✓ | ✓ | – | ✓ | – | positions |
| Tables | ✓ | ✓ | – | ✓ | – | – |
| Render pages | ✓ | ✓ | – | ✓ | – | ✓ |
| OCR | ✓ | ✓ | – | – | – | – |
| Merge, split, reorder, rotate | ✓ | ✓ | ✓ | – | ✓ | partly |
| Text, image and QR stamps | ✓ | ✓ | by overlay | – | by overlay | – |
| Redaction that removes content | ✓ | ✓ | – | – | – | – |
| Redaction verified before writing | ✓ | – | – | – | – | – |
| Replace text in place | ✓ | – | – | – | – | – |
| Fill forms | ✓ | ✓ | ✓ | – | – | – |
| Encrypt and decrypt | ✓ | ✓ | ✓ | – | ✓ | – |
| Add and list annotations | ✓ | ✓ | ✓ | list | – | – |
| Sign digitally | ✓ | – | – | – | – | – |
| Verify signatures | ✓ | – | – | – | – | ✓ |
| Inspect for active content and hidden text | ✓ | – | – | – | – | – |
| Remove active content | ✓ | ✓ | – | – | – | – |
| Memory and time limits per call | ✓ | – | – | – | – | – |
| Create from Markdown | ✓ | from HTML | – | – | – | – |
| Built-in MCP server and tool schemas | ✓ | – | – | – | – | – |
| JSON from every command | ✓ | library | library | library | partly | – |
| Runtime needed | none | Python | Python | Python | none | none |
| License | MIT | AGPL or commercial | BSD | MIT | Apache-2.0 | GPL |

PyMuPDF is the closest in scope and is an excellent library; it is written in C, needs Python, and
its AGPL license matters if you ship it. pdfops trades some breadth for a single MIT-licensed
binary whose tools an agent can call directly.

## Benchmarks

Best of 3 whole-process runs, start-up included, since that is what one tool call costs an agent.
Document: the NASM 2.16 manual, 308 pages and 1.2 MB; `images` on a 352 page, 2.6 MB book with
pictures; forms on a one page form. Machine: 8 core AMD Ryzen 7 9800X3D, Windows 11. Versions:
poppler 25.07, qpdf 12.4, PyMuPDF 1.28, pypdf 6.19, pdfplumber 0.11, pyHanko 0.37 (CLI 0.5),
tesseract 5.5. The fastest entry of each row is bold; `n/a` marks a tool that was installed and
did not complete the task. PyMuPDF's entry for `sanitize` is its `scrub`.

| Task | pdfops | command line tool | PyMuPDF | pypdf | pdfplumber |
| --- | ---: | ---: | ---: | ---: | ---: |
| `info` | **7 ms** | `pdfinfo` 12 ms | 129 ms | 203 ms | 222 ms |
| `text`, all pages | **53 ms** | `pdftotext` 540 ms | 297 ms | 1273 ms | 11.3 s |
| `search`, all pages | **53 ms** | - | 326 ms | - | - |
| `layout`, every word with its box | **194 ms** | - | 346 ms | - | 10.9 s |
| `tables`, 50 pages | **20 ms** | - | 1558 ms | - | 1629 ms |
| `outline` | **15 ms** | - | 128 ms | 221 ms | - |
| `render`, 20 pages at 150 dpi | **41 ms** | `pdftoppm` 2677 ms | 593 ms | - | - |
| `ocr`, one page | 1087 ms | `tesseract` **759 ms** | - | - | - |
| `images`, all embedded images | **230 ms** | `pdfimages` 5621 ms | 1297 ms | 1553 ms | - |
| `create`, 100 sections of Markdown | **12 ms** | - | - | - | - |
| `merge`, three copies | **57 ms** | `qpdf` 204 ms | 401 ms | 2037 ms | - |
| `pages`, keep 10 | **17 ms** | `qpdf` 138 ms | 138 ms | 285 ms | - |
| `split`, one file per page | **156 ms** | `pdfseparate` 46.0 s | 437 ms | 2513 ms | - |
| `rotate`, all pages | **20 ms** | `qpdf` 144 ms | 147 ms | 769 ms | - |
| `stamp`, text on every page | **21 ms** | - | 279 ms | - | - |
| `stamp`, QR code on every page | **121 ms** | - | - | - | - |
| `annotations`, list | **18 ms** | - | 227 ms | - | 309 ms |
| `annotate`, highlight a word on every page | **291 ms** | - | 700 ms | - | - |
| `redact`, a word on every page | **564 ms** | - | 1926 ms | - | - |
| `replace`, a word on every page | **575 ms** | - | - | - | - |
| `scan`, structure and hidden text | **232 ms** | - | - | - | - |
| `scan`, structure only | **16 ms** | - | - | - | - |
| `sanitize` | **32 ms** | - | 2072 ms | - | - |
| `set-meta` | **19 ms** | - | 140 ms | 771 ms | - |
| `compress` | **31 ms** | `qpdf` 179 ms | 378 ms | - | - |
| `encrypt`, AES-256 | **25 ms** | `qpdf` 174 ms | 150 ms | 825 ms | - |
| `decrypt` | **38 ms** | `qpdf` 170 ms | 158 ms | 875 ms | - |
| `sign`, RSA-2048 | **23 ms** | `pyhanko` 615 ms | - | - | - |
| `signatures`, verify | **16 ms** | `pdfsig` n/a | - | - | - |
| `forms`, list fields | **7 ms** | - | 121 ms | 169 ms | - |
| `fill`, one field | **9 ms** | - | 130 ms | 198 ms | - |

Reproduce with `scripts/bench.py` on any document.

Why it is fast: `info`, `layout`, `tables`, `render`, `images` and `ocr` read objects on demand, so
opening a file costs a few milliseconds whatever its size. Text extraction, layout, tables,
rendering, splitting, image export and OCR run on all cores. Page operations copy only the objects
the selected pages reach, so output size and time follow the selection, not the source.

## Using it without MCP

`pdfops tools` prints `[{"name", "description", "inputSchema"}]` for every command. Pass these to
any function calling API, then run the call through the CLI or the library.

```rust
let result = pdfops::tools::call(
    "pdf_text",
    serde_json::json!({"input": "report.pdf", "pages": "1-3"}),
)?;
```

Typed entry points live in `pdfops::ops`, for example `pdfops::ops::read::text(TextArgs { .. })`.

## Behaviour worth knowing

The short version. `pdfops <command> --help` has the detail for each command.

- **Any script.** Text that `stamp`, `fill`, `replace`, `create` and `ocr -o` draw is shaped and
  embedded as font subsets; where no one font has every character, several share the text.
  Hebrew and Arabic are laid out from the right and read back in the order they are read in, so
  a word is found by typing it.
- **Redaction removes, it does not cover.** Glyphs, image pixels in any encoding, drawings and
  annotations under an area are deleted, a text to redact is also struck from metadata and
  bookmarks, and the result is read back by a second interpreter before anything is written.
- **Replace** writes in the document's own font where it has the glyphs and moves the rest of
  the line along. Text does not move to another line: `--dry-run` shows `overflow_pt` first.
- **Dry run.** `redact`, `replace`, `annotate` and `stamp` take `--dry-run`: all the work,
  nothing written.
- **Scan** reports scripts, actions, attachments, disguised content and hidden text by severity.
  It is not a virus scanner and never calls a file safe. **Sanitize** removes the active content
  and scans the result before writing it.
- **Tables** with ruling lines are read cell by cell. Tables without are inferred from alignment
  (`detected_by: alignment`) and deserve a look.
- **OCR** is tesseract's. `ocr -o` writes the recognised text into a copy as an invisible layer.
- **Signatures.** `sign` appends, so earlier signatures stay valid, and can show the signature
  on a page with `--visible`. `signatures` checks that the bytes are unchanged and who signed;
  with `--trust` it also checks the signer's chain against certificates you name. Any other
  change to a signed PDF invalidates its signatures, as it must.
- **Merging and page work** keep bookmarks and links that lead to pages in the output, and
  rename the form fields of later inputs `doc2.<name>` so equal names do not share a value.
- **Damaged files** are repaired for reading and rebuilt for writing; the result then carries
  `repaired_inputs`. **Protected files stay protected** when edited; only `decrypt` removes it.
- **Limits.** 4 GiB and 300 seconds per command by default: `--max-memory`, `--timeout`, or
  `PDFOPS_MAX_MEMORY` and `PDFOPS_TIMEOUT`; 0 lifts a limit. Rasters are capped at 64 megapixels.
- **Not there:** HTML or CSS in `create`, moving text between lines in `replace`, a timestamp
  authority and revocation checks for signatures, editing text inside form fields and
  annotations with `replace`.

## Development

```sh
cargo test                                # under a second; fixtures are generated in memory
cargo test -- --ignored                   # OCR test, needs tesseract with a language pack
cargo clippy --all-targets -- -D warnings
cargo fmt --check
scripts/bench.py --help                   # regenerate the benchmark table
```

Built on [lopdf](https://github.com/J-F-Liu/lopdf) (object model),
[hayro](https://github.com/LaurenzV/hayro) (text, rendering, positions and image decoding),
[rustybuzz](https://github.com/harfbuzz/rustybuzz) (text shaping),
[subsetter](https://github.com/typst/subsetter) (font embedding) and
[pulldown-cmark](https://github.com/pulldown-cmark/pulldown-cmark) (Markdown).

## License

MIT
