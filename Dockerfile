# Headless Linux image — for MCP registry health checks (e.g. Glama's
# "does it start and answer introspection?" Docker probe) and CI, NOT for
# real desktop control. Nova's desktop backends are macOS and Windows; on
# Linux the server starts, answers `initialize`/`tools/list` with the full
# tool catalog, and every desktop action returns a clear "headless build"
# error — see src/platform/headless.rs.

FROM rust:1-slim AS build
WORKDIR /src
COPY . .
RUN cargo build --release --locked

FROM debian:bookworm-slim
COPY --from=build /src/target/release/nova /usr/local/bin/nova
# stdio MCP transport by default; `--http --addr 0.0.0.0:8000` for Streamable HTTP.
ENTRYPOINT ["nova"]
