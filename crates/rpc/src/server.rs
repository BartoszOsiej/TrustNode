//! JSON-RPC server — simplified TCP-based implementation

use std::net::SocketAddr;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

use crate::handler::{RpcHandler, RpcRequest, RpcResponse};

/// JSON-RPC server
pub struct RpcServer {
    handler: Arc<RpcHandler>,
    listen_addr: SocketAddr,
}

impl RpcServer {
    /// Create a new RPC server
    pub fn new(handler: Arc<RpcHandler>, listen_addr: SocketAddr) -> Self {
        Self {
            handler,
            listen_addr,
        }
    }

    /// Start the RPC server
    pub async fn start(&self) -> anyhow::Result<()> {
        let listener = TcpListener::bind(self.listen_addr).await?;
        tracing::info!("RPC server listening on {}", self.listen_addr);

        loop {
            let (mut stream, _addr) = listener.accept().await?;
            let handler = self.handler.clone();

            tokio::spawn(async move {
                let mut buf = vec![0u8; 65536];
                match stream.read(&mut buf).await {
                    Ok(0) => return,
                    Ok(n) => {
                        let body = &buf[..n];
                        // Find the JSON body after HTTP headers
                        if let Some(json_start) = find_json_body(body) {
                            let json_bytes = &body[json_start..];
                            let rpc_response =
                                match serde_json::from_slice::<RpcRequest>(json_bytes) {
                                    Ok(req) => handler.handle(&req),
                                    Err(e) => RpcResponse::error(
                                        None,
                                        crate::handler::RpcError::invalid_params(&format!(
                                            "Invalid JSON: {}",
                                            e
                                        )),
                                    ),
                                };

                            let json = serde_json::to_string(&rpc_response).unwrap();
                            let response = format!(
                                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
                                json.len(),
                                json
                            );
                            let _ = stream.write_all(response.as_bytes()).await;
                        } else {
                            let response = "HTTP/1.1 400 Bad Request\r\n\r\n";
                            let _ = stream.write_all(response.as_bytes()).await;
                        }
                    }
                    Err(_) => {}
                }
            });
        }
    }

    /// Get the listen address
    pub fn listen_addr(&self) -> SocketAddr {
        self.listen_addr
    }
}

/// Find the start of JSON body in an HTTP request
fn find_json_body(data: &[u8]) -> Option<usize> {
    // Look for \r\n\r\n which separates headers from body
    for i in 0..data.len().saturating_sub(3) {
        if &data[i..i + 4] == b"\r\n\r\n" {
            return Some(i + 4);
        }
    }
    // If no HTTP headers, assume the whole thing is JSON
    if !data.is_empty() && data[0] == b'{' {
        return Some(0);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use solana_accounts::store::AccountsDB;

    #[test]
    fn test_rpc_server_creation() {
        let db = Arc::new(AccountsDB::new());
        let handler = Arc::new(RpcHandler::new(db, [0u8; 32]));
        let addr: SocketAddr = "127.0.0.1:8899".parse().unwrap();

        let server = RpcServer::new(handler, addr);
        assert_eq!(server.listen_addr(), addr);
    }
}
