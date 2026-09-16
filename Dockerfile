# syntax=docker/dockerfile:1
FROM rust:1-bookworm AS build
WORKDIR /src
COPY . .
RUN cargo build --release -p clipsync-relay

FROM gcr.io/distroless/cc-debian12
COPY --from=build /src/target/release/clipsync-relay /clipsync-relay
USER 65532:65532
ENV CLIPSYNC_RELAY_BIND=0.0.0.0:7600
EXPOSE 7600
ENTRYPOINT ["/clipsync-relay"]
