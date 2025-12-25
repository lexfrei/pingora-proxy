//! HTTP health server for Kubernetes probes.
//!
//! Provides `/healthz` (liveness) and `/readyz` (readiness) endpoints
//! for Kubernetes health checks.

use std::convert::Infallible;
use std::net::SocketAddr;

use http_body_util::Full;
use hyper::body::Bytes;
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use tokio::net::TcpListener;

/// Handles health check requests.
///
/// Returns 200 "ok" for `/healthz` and `/readyz`.
/// Returns 404 for all other paths.
pub async fn health_handler(
    req: Request<hyper::body::Incoming>,
) -> Result<Response<Full<Bytes>>, Infallible> {
    let response = match req.uri().path() {
        "/healthz" | "/readyz" => Response::builder()
            .status(StatusCode::OK)
            .body(Full::new(Bytes::from("ok")))
            .unwrap(),
        _ => Response::builder()
            .status(StatusCode::NOT_FOUND)
            .body(Full::new(Bytes::from("not found")))
            .unwrap(),
    };
    Ok(response)
}

/// Starts the HTTP health server on the given address.
///
/// Runs indefinitely, accepting connections and handling health requests.
pub async fn start_health_server(addr: SocketAddr) -> std::io::Result<()> {
    let listener = TcpListener::bind(addr).await?;

    loop {
        let (stream, _) = listener.accept().await?;
        let io = TokioIo::new(stream);

        tokio::spawn(async move {
            if let Err(e) = http1::Builder::new()
                .serve_connection(io, service_fn(health_handler))
                .await
            {
                tracing::debug!(error = %e, "health connection error");
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener as StdTcpListener;

    /// Tests health endpoints via real HTTP requests.
    /// We test through the actual server since hyper::body::Incoming
    /// cannot be constructed directly.

    #[tokio::test]
    async fn test_healthz_returns_ok() {
        let listener = StdTcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        drop(listener);

        let handle = tokio::spawn(async move {
            let _ = start_health_server(addr).await;
        });

        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;

        let response = reqwest_get(&format!("http://{}/healthz", addr)).await;
        assert_eq!(response.0, 200);
        assert_eq!(response.1, "ok");

        handle.abort();
    }

    #[tokio::test]
    async fn test_readyz_returns_ok() {
        let listener = StdTcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        drop(listener);

        let handle = tokio::spawn(async move {
            let _ = start_health_server(addr).await;
        });

        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;

        let response = reqwest_get(&format!("http://{}/readyz", addr)).await;
        assert_eq!(response.0, 200);
        assert_eq!(response.1, "ok");

        handle.abort();
    }

    #[tokio::test]
    async fn test_unknown_path_returns_404() {
        let listener = StdTcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        drop(listener);

        let handle = tokio::spawn(async move {
            let _ = start_health_server(addr).await;
        });

        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;

        let response = reqwest_get(&format!("http://{}/foo", addr)).await;
        assert_eq!(response.0, 404);

        handle.abort();
    }

    /// Simple HTTP GET using tokio's TcpStream (no external deps).
    async fn reqwest_get(url: &str) -> (u16, String) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio::net::TcpStream;

        let url = url.strip_prefix("http://").unwrap();
        let (addr, path) = url.split_once('/').unwrap_or((url, ""));
        let path = format!("/{}", path);

        let mut stream = TcpStream::connect(addr).await.unwrap();
        let request = format!(
            "GET {} HTTP/1.1\r\nHost: {}\r\nConnection: close\r\n\r\n",
            path, addr
        );
        stream.write_all(request.as_bytes()).await.unwrap();

        let mut response = String::new();
        stream.read_to_string(&mut response).await.unwrap();

        // Parse status code from "HTTP/1.1 200 OK"
        let status_line = response.lines().next().unwrap();
        let status_code: u16 = status_line
            .split_whitespace()
            .nth(1)
            .unwrap()
            .parse()
            .unwrap();

        // Get body (after \r\n\r\n)
        let body = response
            .split("\r\n\r\n")
            .nth(1)
            .unwrap_or("")
            .to_string();

        (status_code, body)
    }
}
