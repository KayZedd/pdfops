# pdfops

Fast PDF operations for AI agents. A single Rust binary with no native PDF libraries to install, JSON in and out.

- **CLI**: every command prints a single JSON document.
- **MCP server**: `pdfops mcp` exposes every command as a tool over stdio.
- **Function calling**: `pdfops tools` prints tool definitions with JSON Schemas.
- **Library**: the same commands as Rust functions.

## Install

Download a prebuilt binary from the [latest release](https://github.com/KayZedd/pdfops/releases/latest)
and put `pdfops` on your `PATH`. Archives are named by platform:

| Platform | Archive |
| --- | --- |
| Linux x86-64 | `pdfops-<version>-x86_64-unknown-linux-gnu.tar.gz` |
| Linux ARM64 | `pdfops-<version>-aarch64-unknown-linux-gnu.tar.gz` |
| macOS Apple silicon | `pdfops-<version>-aarch64-apple-darwin.tar.gz` |
| macOS Intel | `pdfops-<version>-x86_64-apple-darwin.tar.gz` |
| Windows x86-64 | `pdfops-<version>-x86_64-pc-windows-msvc.zip` |

Or build from source with Rust 1.92 or newer:

```sh
cargo install --git https://github.com/KayZedd/pdfops
```

OCR additionally needs the `tesseract` program; nothing else does.
Language data is fetched on request, without administrator rights:

```sh
pdfops ocr-langs                     # is tesseract there, which languages can be used
pdfops ocr-install --lang pol+eng    # download language data into the user's data directory
pdfops ocr-install --engine          # install tesseract itself where that needs no password
```

## Commands

| Command | What it does |
| --- | --- |
| `info` | Page count, page size, metadata, encryption, outline and form summary |
| `text` | Extract text page by page, with a character budget and optional OCR fallback |
| `search` | Find text or a regex, returns pages and snippets |
| `layout` | Text with positions: bounding box, font and size of every line or word |
| `tables` | Tables as rows of cells, Markdown or CSV |
| `ocr` | Recognise text on scanned pages |
| `ocr-langs` / `ocr-install` | Check the OCR setup, download language data, install tesseract |
| `outline` | Bookmarks with target pages |
| `render` | Pages to PNG, for charts and layout |
| `images` | Extract the images drawn on pages |
| `create` | New PDF from Markdown |
| `merge` | Concatenate PDFs |
| `pages` | Keep, reorder, duplicate or delete pages |
| `split` | Split by page count or by ranges |
| `rotate` | Rotate pages by multiples of 90 degrees |
| `stamp` | Text watermark, header, footer or page numbers; an image such as a signature; a QR code |
| `redact` | Remove text, images and drawings in given areas or matching given text, then verify |
| `replace` | Replace text in place, in the document's own font where possible |
| `set-meta` | Set title, author, subject, keywords, creator |
| `compress` | Shrink: lossless by default, optionally re-encoding and downscaling images |
| `encrypt` / `decrypt` | AES-256 passwords and permissions |
| `forms` / `fill` | List and fill form fields |

Run `pdfops <command> --help` for the options of each.

## Usage

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

$ pdfops text manual.pdf --pages 12- --max-chars 4000     # resume_at_page says where to continue
$ pdfops text scan.pdf --ocr --ocr-lang pol+eng           # OCR only the pages that have no text
$ pdfops render report.pdf --pages 1-3 --dpi 150 -o out/  # look at charts and layout
$ pdfops images report.pdf -o images/
$ pdfops pages in.pdf --keep "3,1,5-" -o out.pdf
$ pdfops split in.pdf --every 10 -o parts/
$ pdfops merge a.pdf b.pdf -o merged.pdf
$ pdfops tables report.pdf --pages 4 --format markdown
$ pdfops layout report.pdf --pages 4 --level words         # bbox, font and size per word
$ pdfops stamp in.pdf --text "Poufne · {page}/{pages}" --position footer -o out.pdf
$ pdfops stamp in.pdf --image signature.png --x 380 --y 690 --width 140 --pages last -o out.pdf
$ pdfops stamp in.pdf --qr "https://example.com/doc/42" --anchor bottom-right -o out.pdf
$ pdfops redact in.pdf --text "Jan Kowalski" --text "\d{11}" --regex -o redacted.pdf
$ pdfops redact in.pdf --rect "2:100,200,300,220" -o redacted.pdf
$ pdfops replace in.pdf --find "2025" --with "2026" -o out.pdf
$ pdfops create notes.md -o notes.pdf
$ pdfops compress in.pdf --max-image-edge 1600 --image-quality 70 -o small.pdf
$ pdfops forms form.pdf
$ pdfops fill form.pdf --set name="Ada Lovelace" --set agree=true -o filled.pdf
$ pdfops encrypt in.pdf --owner-password secret --deny-copy -o locked.pdf
```

Positions are in points with the origin at the top-left corner of the page as displayed, y growing
downwards. `layout` reports them, `stamp --x/--y` and `redact --rect` accept them, and a pixel of
`render --dpi 72` is exactly one point.

Page specs are 1-based and comma separated: `3`, `2-5`, `7-` (to the end), `-4` (from the start),
`5-2` (descending), `last`, `odd`, `even`, `all`.

Commands that write a file take `-o`; it may be the input file, since output goes through a
temporary file. Errors are printed to stderr as `{"error": "..."}` with exit status 1. Add
`--pretty` to indent the JSON.

## MCP server

```json
{
  "mcpServers": {
    "pdfops": { "command": "pdfops", "args": ["mcp"] }
  }
}
```

For Claude Code: `claude mcp add pdfops -- pdfops mcp`.

Tools are named `pdf_info`, `pdf_text`, `pdf_set_meta` and so on, and take the same arguments as
the CLI. Relative paths resolve against the server's working directory.

## Function calling without MCP

`pdfops tools` prints `[{"name", "description", "inputSchema"}]` for every command. Pass these to
any function calling API, then run the call through the CLI or the library.

## Library

```rust
let result = pdfops::tools::call(
    "pdf_text",
    serde_json::json!({"input": "report.pdf", "pages": "1-3"}),
)?;
```

Typed entry points live in `pdfops::ops`, for example `pdfops::ops::read::text(TextArgs { .. })`.

## Performance

Best of 5 runs on a 357 page, 1.4 MB manual, 4 core Intel i5-4460, against poppler 26 and qpdf 12:

| Task | pdfops | Reference tool |
| --- | ---: | ---: |
| Info | 7 ms | `pdfinfo` 16 ms |
| Text, all pages | 466 ms | `pdftotext` 565 ms |
| Keep 10 pages | 32 ms | `qpdf` 164 ms |
| Split into single pages | 64 ms | `pdfseparate` over 40 s |
| Merge three copies | 164 ms | `qpdf` 314 ms |
| Render 20 pages at 150 dpi | 214 ms | `pdftoppm` 6125 ms |

`info`, `render`, `images` and `ocr` read objects on demand, so opening a file costs a few
milliseconds whatever its size. Text extraction, rendering, splitting, image export and OCR run on
all cores. Page operations copy only the objects the selected pages reach, so output size and time
follow the selection, not the source.

## Behaviour worth knowing

- **Text outside Latin-1.** `stamp`, `fill`, `replace` and `create` embed a subset of a font that has the glyphs: the one
  given with `stamp --font`, otherwise one found on the system. Latin-1 text uses the built-in
  Helvetica and embeds nothing. Text is placed glyph by glyph: scripts that need shaping or
  right-to-left layout (Arabic, Hebrew, Indic) will not come out right.
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
- **Tables** drawn with ruling lines are read cell by cell and are reliable. Tables without lines
  are inferred from column alignment (`detected_by: alignment`) and deserve a look before trusting.
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
- **Signatures.** Any change to a signed PDF invalidates its digital signatures.

## Development

```sh
cargo test                                # under a second; fixtures are generated in memory
cargo test -- --ignored                   # OCR test, needs tesseract with a language pack
cargo clippy --all-targets -- -D warnings
cargo fmt --check
```

Built on [lopdf](https://github.com/J-F-Liu/lopdf) (object model),
[pdf-extract](https://github.com/jrmuizel/pdf-extract) (text),
[hayro](https://github.com/LaurenzV/hayro) (rendering, positions and image decoding),
[subsetter](https://github.com/typst/subsetter) (font embedding) and
[pulldown-cmark](https://github.com/pulldown-cmark/pulldown-cmark) (Markdown).

## License

MIT
