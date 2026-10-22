# pdfctl with PDFium and qpdf bundled.
#
#   docker build -t pdf-form-filler .
#   mkdir -p out && chmod 777 out
#   docker run --rm -v "$PWD:/work:ro" -v "$PWD/out:/out" pdf-form-filler \
#     fill --form /work/fixtures/forms/ga-t7-bill-of-sale.pdf --data /work/examples/ga-t7-bill-of-sale.json --out /out/out.pdf
#   docker build --target test .     # runs the full test suite against the bundled PDFium
#
# Stages: compile, fetch a pinned and checksummed PDFium (bblanchon/pdfium-binaries,
# the build pdfium-render 0.9 targets), then a slim runtime with qpdf running as
# a non-root user.

FROM rust:1.90-bookworm AS build
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY src ./src
COPY tests ./tests
RUN cargo build --release --locked --bin pdfctl

FROM debian:bookworm-slim AS pdfium
ARG PDFIUM_BUILD=7881
ARG TARGETARCH=amd64
RUN apt-get update \
    && apt-get install --yes --no-install-recommends ca-certificates curl \
    && rm -rf /var/lib/apt/lists/*
RUN set -eu; \
    case "$TARGETARCH" in \
      amd64) asset=pdfium-linux-x64.tgz; sum=1470e21b8b4a3b4ad7f85684e2da11d94f3b69a86d81dee11b9b6709d927ac1d ;; \
      arm64) asset=pdfium-linux-arm64.tgz; sum=ee7f7b7d5468958336a818c1cd580bdd20972846b7377b13f9a923d92d1d4674 ;; \
      *) echo "unsupported architecture: $TARGETARCH" >&2; exit 1 ;; \
    esac; \
    curl -fsSL -o /tmp/pdfium.tgz "https://github.com/bblanchon/pdfium-binaries/releases/download/chromium%2F${PDFIUM_BUILD}/${asset}"; \
    echo "${sum}  /tmp/pdfium.tgz" | sha256sum -c -; \
    mkdir -p /opt/pdfium; \
    tar -xzf /tmp/pdfium.tgz -C /opt/pdfium lib/libpdfium.so LICENSE

# `docker build --target test .` — the corpus, process and unit tests, PDFium required.
FROM build AS test
RUN apt-get update \
    && apt-get install --yes --no-install-recommends qpdf \
    && rm -rf /var/lib/apt/lists/*
COPY --from=pdfium /opt/pdfium /opt/pdfium
COPY fixtures ./fixtures
ENV PDFIUM_DYNAMIC_LIB_PATH=/opt/pdfium/lib REQUIRE_PDFIUM=1
RUN cargo test --release --locked

FROM debian:bookworm-slim
RUN apt-get update \
    && apt-get install --yes --no-install-recommends qpdf \
    && rm -rf /var/lib/apt/lists/*
COPY --from=pdfium /opt/pdfium/lib/libpdfium.so /opt/pdfium/lib/libpdfium.so
COPY --from=pdfium /opt/pdfium/LICENSE /usr/share/doc/pdfium/LICENSE
COPY --from=build /src/target/release/pdfctl /usr/local/bin/pdfctl
ENV PDFIUM_DYNAMIC_LIB_PATH=/opt/pdfium/lib \
    QPDF_PATH=/usr/bin/qpdf
USER 65532:65532
ENTRYPOINT ["pdfctl"]
