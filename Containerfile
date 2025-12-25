FROM docker.io/library/rust:1.92-alpine3.20 AS builder

# Build dependencies for static linking and protobuf
# hadolint ignore=DL3018
RUN apk add --no-cache musl-dev protobuf-dev cmake g++ make

WORKDIR /build

# Copy proto files first (changes rarely)
COPY proto proto

# Copy Cargo manifests for better caching
COPY Cargo.toml Cargo.lock ./

# Copy build script and source
COPY build.rs ./
COPY src src

# Build release binary
RUN cargo build --release

# Create passwd for non-root user
RUN echo 'nobody:x:65534:65534:Nobody:/:' > /tmp/passwd

FROM scratch

COPY --from=builder /tmp/passwd /etc/passwd
COPY --from=builder /etc/ssl/certs/ca-certificates.crt /etc/ssl/certs/
COPY --from=builder --chmod=555 /build/target/release/pingora-proxy /pingora-proxy

USER 65534
EXPOSE 8080/tcp 9090/tcp 50051/tcp
ENTRYPOINT ["/pingora-proxy"]
