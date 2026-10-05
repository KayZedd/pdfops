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
| | `ocr` | Recognised text of scanned pages |
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
pdfops signatures signed.pdf                            # valid, unchanged, who and when
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
Document: a 357 page, 1.4 MB manual; `images` on a 4.6 MB manual with pictures; forms on a one page
form. Machine: 4 core Intel i5-4460. Versions: poppler 26.08, qpdf 12.4, PyMuPDF 1.28, pypdf 6.19,
pdfplumber 0.11, pyHanko 0.5 (CLI), tesseract 5.5. The fastest entry of each row is bold.

| Task | pdfops | command line tool | PyMuPDF | pypdf | pdfplumber |
| --- | ---: | ---: | ---: | ---: | ---: |
| `info` | **5 ms** | `pdfinfo` 13 ms | 211 ms | 454 ms | 371 ms |
| `text`, all pages | **483 ms** | `pdftotext` 541 ms | 641 ms | 4146 ms | 34.5 s |
| `search`, all pages | **430 ms** | - | 627 ms | - | - |
| `layout`, every word with its box | **548 ms** | - | 758 ms | - | 33.0 s |
| `tables`, 50 pages | **52 ms** | - | 4422 ms | - | 4133 ms |
| `outline` | **20 ms** | - | 201 ms | 353 ms | - |
| `render`, 20 pages at 150 dpi | **195 ms** | `pdftoppm` 5962 ms | 1229 ms | - | - |
| `ocr`, one page (render, then tesseract) | 2338 ms | `tesseract` **1511 ms** | - | - | - |
| `images`, all embedded images | **391 ms** | `pdfimages` 3897 ms | 675 ms | 5232 ms | - |
| `create`, 100 sections of Markdown | **10 ms** | - | - | - | - |
| `merge`, three copies | **123 ms** | `qpdf` 254 ms | 1183 ms | 3825 ms | - |
| `pages`, keep 10 | **21 ms** | `qpdf` 122 ms | 204 ms | 419 ms | - |
| `split`, one file per page | **54 ms** | `pdfseparate` over 60 s | 877 ms | 2492 ms | - |
| `rotate`, all pages | **47 ms** | `qpdf` 152 ms | 252 ms | 1309 ms | - |
| `stamp`, text on every page | **33 ms** | - | 610 ms | - | - |
| `stamp`, QR code on every page | **180 ms** | - | - | - | - |
| `redact`, a word on every page | 2144 ms | - | **1532 ms** | - | - |
| `replace`, a word on every page | **1736 ms** | - | - | - | - |
| `set-meta` | **60 ms** | - | 269 ms | 1416 ms | - |
| `compress` | **65 ms** | `qpdf` 274 ms | 1109 ms | - | - |
| `encrypt`, AES-256 | **49 ms** | `qpdf` 160 ms | 253 ms | 1715 ms | - |
| `decrypt` | **76 ms** | `qpdf` 178 ms | 397 ms | 2418 ms | - |
| `forms`, list fields | **15 ms** | - | 391 ms | 236 ms | - |
| `fill`, one field | **7 ms** | - | 509 ms | 433 ms | - |

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

- **Text outside Latin-1.** `stamp`, `fill`, `replace` and `create` embed a subset of a font that
  has the glyphs: the one given with `stamp --font`, otherwise one found on the system. Latin-1 text uses the built-in
  Helvetica and embeds nothing. Embedded text is shaped: Arabic is joined, Hebrew and Arabic run
  from the right, Indic and Thai clusters are formed and their marks placed, and Latin gets its
  ligatures and kerning. Three limits: one font must cover all the text of a style, `create` lays
  out word by word, so punctuation next to a right-to-left word may land on its other side and such
  lines keep their left edge, and reading the text back gives it in drawing order, right-to-left
  words reversed and some Indic and Thai clusters with their characters regrouped.
- **Redaction** deletes what is under the areas from the page content: glyphs, image pixels
  (including scanned pages), drawings lying wholly inside an area, and annotations with the form
  values they show. Text around it does not move. The result is read back by a second, independent
  interpreter, and nothing is written unless every area is empty. When a page is built in a way that
  cannot be rewritten with certainty (inline images, JPEG 2000 or CMYK JPEG under an area), the
  command fails instead of guessing. It does not rewrite document metadata, bookmarks or attached
  files, and it removes the accessibility structure tree, which can repeat page text.
- **Replace** writes the new text with the codes the document's own font already uses for those
  characters on that page, so style is kept exactly. If the font (usually a subset) lacks a needed
  glyph, another font writes just those words, at the same size, position and colour, and the result
  says so. Lines are not re-flowed: a longer replacement runs into what follows, and `overflow_pt`
  reports by how much. Text inside form fields and annotations is not edited.
- **Dry run.** `redact`, `replace`, `annotate` and `stamp` take `--dry-run` (`dry_run` over MCP):
  the command does all of its work, including the check after redaction, reports what it would
  change and writes nothing. `redact` lists every area with the text that matched and the counts of
  glyphs, images, drawings and annotations that would go; `replace` lists every match with its box,
  the old and new text, which font would write it and by how much it would overflow.
- **Scan** inspects structure; it is not a virus scanner and never calls a file safe. The verdict is
  `nothing found` or `findings`, each finding has a severity (`high`, `medium`, `low`, `info`), the
  objects it sits in and samples, and the result lists what was checked and what was not. It reads
  the raw bytes as well as the parsed objects, so a file that does not open, or hides a name behind
  `#xx` escapes or inside a packed object, is still judged. With `--clamav` the file is also passed
  to `clamscan` when that is installed; no signature database is bundled.
- **Hidden text** is found by looking: every page is rendered, and a word that leaves no trace in
  the picture is reported with its page, box, reason and text, whether it is in the invisible text
  mode, in the colour of its background, under a flat shape, clipped away, smaller than 1.5 points
  or off the page. The judgement is made from pdfops' own rendering, and is withheld where that
  cannot be relied on: under translucent or blended drawing, and for fonts it cannot draw. Rendering runs in a process of its own, so a file built to exhaust memory or
  time costs this one check and is reported as `resource_exhaustion`; the rest of the result stands. Invisible text over visible content, which scanned pages with recognised text
  have, is reported apart as `invisible_text_layer` with severity `info`. Text under a picture that
  is not a flat colour is not detected, and neither is text inside annotations and form fields. The boxes can be passed to `redact --rect`.
- **Sanitize** removes JavaScript, actions that run by themselves or start programs, send form
  data or open other files, embedded files, XFA forms and media annotations; `--keep` leaves a
  group in place. Ordinary web links stay, and hidden text is not touched. The result is scanned
  before it is written, and nothing is written if any of it is still there. Like every rewrite, it
  invalidates digital signatures.
- **Tables** drawn with ruling lines are read cell by cell and are reliable. Tables without lines
  are inferred from column alignment (`detected_by: alignment`): columns are the stretches of the
  page that rows fill, kept apart even where a heading lies across two of them; a label or a
  description that wraps is joined to its row; rows with empty cells, a heading over a group of
  rows and a figure set alone under its column stay in the table; running text between two tables
  splits them, and a list of contents with dot leaders is not a table. It is still inference:
  whether a line continues the row above is judged from indentation, spacing and capitals, so such
  tables deserve a look before trusting.
- **Create** understands headings, emphasis, links, nested lists, quotes, code blocks, tables, rules
  and local images. HTML inside the Markdown is ignored; there is no HTML or CSS engine.
- **Bookmarks.** `merge`, `pages` and `split` keep the bookmarks whose target page is in the output.
  They land at the top of that page. Named destinations used by links are not carried over.
- **Forms when merging.** Fields of the second and later inputs are renamed `doc2.<name>`,
  `doc3.<name>`, so equal names do not share a value.
- **Lossy compression** is opt-in through `--image-quality` and `--max-image-edge`. It re-encodes
  8-bit grey and RGB images as JPEG. Masks, palette images, CMYK and 1-bit scans are left untouched,
  and the output is never larger than the input.
- **Images** are the ones a page actually draws, including inline images and images inside forms.
  JPEG and JPEG 2000 are written as stored; everything else (Flate, LZW, CCITT fax, JBIG2, palette,
  masks) is decoded to PNG.
- **OCR** quality and languages are tesseract's. Recognised text is returned, not written into the
  PDF. When a language is missing, the error names the `ocr-install` call that fixes it, so an agent
  can recover on its own. Data comes from the `tessdata_fast` repository (`--best` for the larger
  models) and is kept in `~/.local/share/pdfops/tessdata`, or `PDFOPS_TESSDATA` if set. Installing
  tesseract itself uses the system package manager; where that needs a password, the command to
  run is returned instead.
- **Limits.** A command may hold 4 GiB and run for 300 seconds by default; `--max-memory` (MiB) and
  `--timeout` (seconds), or `PDFOPS_MAX_MEMORY` and `PDFOPS_TIMEOUT`, change that, and 0 lifts a
  limit. Hitting one is an ordinary error. Rasters are capped at 64 megapixels.
- **Damaged files** are read by a parser that repairs them, so `text`, `tables`, `render` and the
  other reading commands work. Commands that write rebuild such a file from what that parser sees:
  every object the catalog and the pages reach, with the page tree laid out afresh. The result then
  carries `repaired_inputs`, and what could not be read is missing from the output, exactly as it
  is from `text` and `render`. A damaged file that is also encrypted, or in which no page can be
  read, is refused. Signing a rebuilt file rewrites it, so signatures it carried do not survive.
- **Protected files stay protected.** Editing an encrypted file writes it back encrypted with the
  same passwords and permissions. Only `decrypt` removes protection.
- **Signing** appends to the file, so signatures already present stay valid. The signature is
  invisible; combine it with `stamp --image` for a visible mark, stamping first. Keys are RSA or
  ECDSA P-256, from PEM files or a PKCS #12 file. There is no timestamp authority and no long-term
  validation data.
- **Verifying** establishes two things: the signed bytes are unchanged, and the signature was made
  by the embedded certificate's key. It does not decide whether that certificate is trustworthy;
  pdfops has no list of certificate authorities. `covers_whole_document` is false when content was
  appended after signing, as a later signature legitimately does.
- **Any other change to a signed PDF invalidates its signatures**, as it must.
- **Annotations** carry their own appearance, so they show in every viewer and in `render`.
  `annotations` lists markup and links; form fields are listed by `forms`.

## Development

```sh
cargo test                                # under a second; fixtures are generated in memory
cargo test -- --ignored                   # OCR test, needs tesseract with a language pack
cargo clippy --all-targets -- -D warnings
cargo fmt --check
scripts/bench.py --help                   # regenerate the benchmark table
```

Built on [lopdf](https://github.com/J-F-Liu/lopdf) (object model),
[pdf-extract](https://github.com/jrmuizel/pdf-extract) (text),
[hayro](https://github.com/LaurenzV/hayro) (rendering, positions and image decoding),
[subsetter](https://github.com/typst/subsetter) (font embedding) and
[pulldown-cmark](https://github.com/pulldown-cmark/pulldown-cmark) (Markdown).

## License

MIT
