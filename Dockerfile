# pdfops with OCR ready to use:
#   docker run --rm -i -v "$PWD:/work" ghcr.io/kayzedd/pdfops mcp --root /work
FROM rust:1-alpine AS build
RUN apk add --no-cache musl-dev
WORKDIR /src
COPY . .
RUN cargo build --release --locked

FROM alpine:3.22
LABEL org.opencontainers.image.source="https://github.com/KayZedd/pdfops" \
      org.opencontainers.image.description="Fast PDF tools for AI agents: CLI and MCP server" \
      org.opencontainers.image.licenses="MIT"
# English OCR works out of the box; `pdfops ocr-install --lang <codes>` adds other languages.
RUN apk add --no-cache tesseract-ocr tesseract-ocr-data-eng
COPY --from=build /src/target/release/pdfops /usr/local/bin/pdfops
WORKDIR /work
ENTRYPOINT ["pdfops"]
