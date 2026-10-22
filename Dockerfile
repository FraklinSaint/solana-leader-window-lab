# syntax=docker/dockerfile:1.7
# ---- Rust build (rust:1.97.1-slim-bookworm, pinned by digest) ----
FROM rust:1.97.1-slim-bookworm@sha256:2775a09d208ff0d7c1f50490c45b62db929e87ba1dcbc3f2132ac71a704bcdd3 AS rust-build
WORKDIR /src
COPY rust-toolchain.toml Cargo.toml Cargo.lock ./
COPY crates ./crates
RUN cargo build --release --locked --bin sender

# ---- sender runtime (debian:bookworm-slim, pinned by digest) ----
FROM debian:bookworm-slim@sha256:7ff6ba31527b9e336162fa061746c366c4b2032f4d2cff2078bea43f78efe3f8 AS sender
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates \
 && rm -rf /var/lib/apt/lists/* \
 && useradd --uid 10001 --create-home --shell /usr/sbin/nologin lab
COPY --from=rust-build /src/target/release/sender /usr/local/bin/sender
WORKDIR /lab
USER lab
ENTRYPOINT ["sender"]
CMD ["plan"]

# ---- analysis (python:3.12.12-slim-bookworm, pinned by digest) ----
FROM python:3.12.12-slim-bookworm@sha256:593bd06efe90efa80dc4eee3948be7c0fde4134606dd40d8dd8dbcade98e669c AS analysis
ENV PYTHONDONTWRITEBYTECODE=1 PYTHONUNBUFFERED=1 PIP_NO_CACHE_DIR=1
COPY analysis/requirements.lock /tmp/requirements.lock
RUN pip install --require-hashes --no-deps -r /tmp/requirements.lock \
 && useradd --uid 10001 --create-home --shell /usr/sbin/nologin lab
WORKDIR /lab
COPY analysis ./analysis
COPY scripts ./scripts
COPY crates/common/src/guard.rs ./crates/common/src/guard.rs
ENV PYTHONPATH=/lab/analysis
USER lab
CMD ["python", "-m", "pytest", "analysis"]
