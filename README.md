# pdfops

Fast PDF operations for AI agents. A single Rust binary with no native PDF libraries to install, JSON in and out.

- **CLI**: every command prints a single JSON document.
- **MCP server**: `pdfops mcp` exposes every command as a tool over stdio.
- **Function calling**: `pdfops tools` prints tool definitions with JSON Schemas.
- **Library**: the same commands as Rust functions.

## Install

```sh
cargo install --git https://github.com/KayZedd/pdfops
```

Requires Rust 1.92 or newer.

## Commands

| Command | What it does |
| --- | --- |
| `info` | Page count, page size, metadata, encryption, outline and form summary |
| `text` | Extract text page by page, with an optional character budget |
| `search` | Find text or a regex, returns pages and snippets |
| `outline` | Bookmarks with target pages |
| `render` | Pages to PNG, for scans, charts and layout |
| `images` | Extract embedded images |
| `merge` | Concatenate PDFs |
| `pages` | Keep, reorder, duplicate or delete pages |
| `split` | Split by page count or by ranges |
| `rotate` | Rotate pages by multiples of 90 degrees |
| `stamp` | Text watermark, header or footer, with page numbers |
| `set-meta` | Set title, author, subject, keywords, creator |
| `compress` | Lossless size reduction |
| `encrypt` / `decrypt` | AES-256 passwords and permissions |
| `forms` / `fill` | List and fill form fields |

Run `pdfops <command> --help` for the options of each.

## Usage

```sh
$ pdfops info manual.pdf
{"encrypted":false,"file":"manual.pdf","form_fields":0,"metadata":{"producer":"GPL Ghostscript 10.07.1"},
 "outline_entries":640,"page_size_pt":{"height":792.0,"width":595.0},"pages":357,"pdf_version":"1.3",
 "size_bytes":1386723,"uniform_page_size":true}

$ pdfops search manual.pdf "calling convention" --max-results 1 --context 40
{"file":"manual.pdf","matches":[{"match":"Calling Convention","page":12,
 "snippet":"... 10.5.1 The Pascal Calling Convention ..."}],"query":"calling convention",
 "total_matches":17,"unreadable_pages":[]}

$ pdfops text manual.pdf --pages 12- --max-chars 4000     # resume_at_page says where to continue
$ pdfops render scan.pdf --pages 1-3 --dpi 150 -o out/    # look at pages that have no text layer
$ pdfops pages in.pdf --keep "3,1,5-" -o out.pdf
$ pdfops split in.pdf --every 10 -o parts/
$ pdfops merge a.pdf b.pdf -o merged.pdf
$ pdfops stamp in.pdf --text "Page {page} of {pages}" --position footer -o out.pdf
$ pdfops forms form.pdf
$ pdfops fill form.pdf --set name="Ada Lovelace" --set agree=true -o filled.pdf
$ pdfops encrypt in.pdf --owner-password secret --deny-copy -o locked.pdf
```

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
| Info | 32 ms | `pdfinfo` 16 ms |
| Text, all pages | 464 ms | `pdftotext` 565 ms |
| Keep 10 pages | 32 ms | `qpdf` 164 ms |
| Split into single pages | 64 ms | `pdfseparate` over 40 s |
| Merge three copies | 114 ms | `qpdf` 314 ms |
| Render 20 pages at 150 dpi | 264 ms | `pdftoppm` 6125 ms |

Text extraction, rendering, splitting and image export run on all cores. Page operations copy only
the objects the selected pages reach, so output size and time follow the selection, not the source.

## Limits

- No OCR. Scanned pages return empty text; use `render` and read the image.
- `stamp` and the appearances written by `fill` use the built-in Helvetica font, so Latin-1 text
  only. `fill` still stores any Unicode value and asks the viewer to draw it.
- `merge`, `pages` and `split` drop the outline. When merging, form fields of the second and later
  inputs are renamed `doc2.<name>`, `doc3.<name>` so equal names do not share a value.
- `compress` is lossless: it does not downsample images.
- `images` exports JPEG and JPEG 2000 as they are and 8-bit raw images as PNG. Other encodings
  (CCITT, JBIG2, indexed colour) are listed as skipped.
- Any change to a signed PDF invalidates its digital signatures.

## Development

```sh
cargo test                                # under a second; fixtures are generated in memory
cargo clippy --all-targets -- -D warnings
cargo fmt --check
```

Built on [lopdf](https://github.com/J-F-Liu/lopdf) (object model),
[pdf-extract](https://github.com/jrmuizel/pdf-extract) (text) and
[hayro](https://github.com/LaurenzV/hayro) (rendering).

## License

MIT
