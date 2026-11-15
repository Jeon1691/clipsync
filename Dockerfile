# syntax=docker/dockerfile:1
FROM rust:1-bookworm@sha256:e6f2cff35d0f309acf8433b4f05878ed77092602c5065cc81801177da0bb3a9c AS build
WORKDIR /src
COPY . .
RUN cargo build --release --locked -p clipsync-relay

FROM gcr.io/distroless/cc-debian12:latest@sha256:e5d81ddde149641e2a9ba55be4545bc125c67de07508b03ba4c22e6eb0ded5aa
COPY --from=build /src/target/release/clipsync-relay /clipsync-relay
USER 65532:65532
ENV CLIPSYNC_RELAY_BIND=0.0.0.0:7600
EXPOSE 7600
ENTRYPOINT ["/clipsync-relay"]
