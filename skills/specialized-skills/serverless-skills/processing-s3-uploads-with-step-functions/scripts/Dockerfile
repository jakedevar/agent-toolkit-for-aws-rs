# Fargate processor image: a Rust release build in one stage, a distroless
# runtime in the next. Both base images are pinned by digest and the dependency
# set by the checksummed Cargo.lock (`--locked`).
#
# Build from this directory:  docker build --platform linux/amd64 -t NAME .
FROM rust:1.94.1-slim-bookworm@sha256:cf9dd0ec73e75f827fe59123fff9dc65af1a1c8363c3c31ee8d7f8ad0b6a5fb2 AS build

WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY src ./src
# The Fargate binary does not need the Lambda runtime (`lambda` feature).
RUN cargo build --release --locked --no-default-features --bin fargate-processor

FROM gcr.io/distroless/cc-debian12:nonroot@sha256:9dac0a79194e45a7da0158a9c6da57b217585af0786db3845d1f0ec1a0dd182f

COPY --from=build /src/target/release/fargate-processor /app/fargate-processor

# Unprivileged user (distroless "nonroot"); /tmp is writable for downloads.
USER 65532:65532
WORKDIR /app
ENTRYPOINT ["/app/fargate-processor"]
