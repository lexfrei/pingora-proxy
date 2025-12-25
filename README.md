# Pingora Proxy

Pingora-based reverse proxy with gRPC API for dynamic route updates.

## Overview

This is the data plane component of the [Pingora Gateway Controller](https://github.com/lexfrei/pingora-gateway-controller). It receives routing configuration from the Kubernetes controller via gRPC and dynamically updates its routing table without restart.

## Features

- Built on [Pingora](https://github.com/cloudflare/pingora) - Cloudflare's battle-tested HTTP proxy framework
- gRPC API for dynamic route updates
- HTTP and gRPC traffic proxying
- Prometheus metrics

## Building

```bash
cargo build --release
```

## Running

```bash
RUST_LOG=info ./target/release/pingora-proxy
```

## Configuration

The proxy is configured dynamically via gRPC from the controller. See the controller documentation for details.

## License

BSD 3-Clause License - see [LICENSE](LICENSE) for details.
