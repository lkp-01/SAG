FROM rust:1.94

# Shared build dependencies for SAG Rust services.
# - protoc: required by sag-tunnel-proto (prost-build)
# - cmake/build-essential/pkg-config: required by native deps (e.g. libz-ng-sys) and crates with build scripts
RUN apt-get update \
  && apt-get install -y --no-install-recommends \
    ca-certificates \
    git \
    protobuf-compiler \
    cmake \
    build-essential \
    pkg-config \
  && rm -rf /var/lib/apt/lists/*

