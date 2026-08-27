//! RPC request/response handler

use serde::{Deserialize, Serialize};
use serde_json::Value;
use solana_accounts::account::Pubkey;
use solana_accounts::store::AccountsDB;
use std::sync::Arc;

/// JSON-RPC 2.0 request
#[derive(Debug, Clone, Deserialize)]
pub struct RpcRequest {
    pub jsonrpc: String,
    pub method: String,
    pub params: Option<Value>,
    pub id: Option<Value>,
}

/// JSON-RPC 2.0 response
#[derive(Debug, Clone, Serialize)]
pub struct RpcResponse {
    pub jsonrpc: String,
    pub result: Option<Value>,
    pub error: Option<RpcError>,
    pub id: Option<Value>,
}

/// JSON-RPC error
#[derive(Debug, Clone, Serialize)]
pub struct RpcError {
    pub code: i32,
    pub message: String,
    pub data: Option<Value>,
}

impl RpcError {
    pub fn method_not_found(method: &str) -> Self {
        Self {
            code: -32601,
            message: format!("Method not found: {}", method),
            data: None,
        }
    }

    pub fn invalid_params(msg: &str) -> Self {
        Self {
            code: -32602,
            message: msg.to_string(),
            data: None,
        }
    }

    pub fn internal_error(msg: &str) -> Self {
        Self {
            code: -32603,
            message: msg.to_string(),
            data: None,
        }
    }
}

impl RpcResponse {
    pub fn success(id: Option<Value>, result: Value) -> Self {
        Self {
            jsonrpc: "2.0".to_string(),
            result: Some(result),
            error: None,
            id,
        }
    }

    pub fn error(id: Option<Value>, error: RpcError) -> Self {
        Self {
            jsonrpc: "2.0".to_string(),
            result: None,
            error: Some(error),
            id,
        }
    }
}

/// RPC method handler
pub struct RpcHandler {
    accounts: Arc<AccountsDB>,
    slot: parking_lot::RwLock<u64>,
    identity: Pubkey,
}

impl RpcHandler {
    /// Create a new RPC handler
    pub fn new(accounts: Arc<AccountsDB>, identity: Pubkey) -> Self {
        Self {
            accounts,
            slot: parking_lot::RwLock::new(0),
            identity,
        }
    }

    /// Handle an RPC request
    pub fn handle(&self, request: &RpcRequest) -> RpcResponse {
        match request.method.as_str() {
            // Cluster info
            "getHealth" => self.get_health(request),
            "getVersion" => self.get_version(request),
            "getIdentity" => self.get_identity(request),
            "getSlot" => self.get_slot(request),
            "getClusterNodes" => self.get_cluster_nodes(request),

            // Account methods
            "getAccountInfo" => self.get_account_info(request),
            "getBalance" => self.get_balance(request),
            "getAccountCount" => self.get_account_count(request),

            // Transaction methods
            "sendTransaction" => self.send_transaction(request),
            "getTransaction" => self.get_transaction(request),

            // Block methods
            "getBlockHeight" => self.get_block_height(request),

            // Epoch methods
            "getEpochInfo" => self.get_epoch_info(request),

            _ => RpcResponse::error(
                request.id.clone(),
                RpcError::method_not_found(&request.method),
            ),
        }
    }

    fn get_health(&self, req: &RpcRequest) -> RpcResponse {
        RpcResponse::success(req.id.clone(), serde_json::json!("ok"))
    }

    fn get_version(&self, req: &RpcRequest) -> RpcResponse {
        RpcResponse::success(
            req.id.clone(),
            serde_json::json!({
                "solana-core": "0.1.0-custom",
                "feature-set": "custom-validator"
            }),
        )
    }

    fn get_identity(&self, req: &RpcRequest) -> RpcResponse {
        RpcResponse::success(
            req.id.clone(),
            serde_json::json!({
                "identity": hex::encode(self.identity)
            }),
        )
    }

    fn get_slot(&self, req: &RpcRequest) -> RpcResponse {
        let slot = *self.slot.read();
        RpcResponse::success(req.id.clone(), serde_json::json!(slot))
    }

    fn get_cluster_nodes(&self, req: &RpcRequest) -> RpcResponse {
        // Return just this validator for now
        RpcResponse::success(
            req.id.clone(),
            serde_json::json!([{
                "pubkey": hex::encode(self.identity),
                "gossip": "127.0.0.1:8000",
                "tpu": "127.0.0.1:8001",
                "rpc": "127.0.0.1:8002",
                "version": "0.1.0-custom",
                "featureSet": 0,
                "shredVersion": 0
            }]),
        )
    }

    fn get_account_info(&self, req: &RpcRequest) -> RpcResponse {
        let params = req.params.as_ref().and_then(|p| p.as_array());
        let pubkey_str = match params.and_then(|p| p.first()).and_then(|p| p.as_str()) {
            Some(s) => s,
            None => {
                return RpcResponse::error(
                    req.id.clone(),
                    RpcError::invalid_params("Missing pubkey parameter"),
                );
            }
        };

        let pubkey_bytes = match hex::decode(pubkey_str) {
            Ok(bytes) if bytes.len() == 32 => {
                let mut key = [0u8; 32];
                key.copy_from_slice(&bytes);
                key
            }
            _ => {
                return RpcResponse::error(
                    req.id.clone(),
                    RpcError::invalid_params("Invalid pubkey"),
                );
            }
        };

        match self.accounts.load(&pubkey_bytes) {
            Some(account) => RpcResponse::success(
                req.id.clone(),
                serde_json::json!({
                    "lamports": account.lamports,
                    "owner": hex::encode(account.owner),
                    "executable": account.executable,
                    "rentEpoch": account.rent_epoch,
                }),
            ),
            None => RpcResponse::success(req.id.clone(), Value::Null),
        }
    }

    fn get_balance(&self, req: &RpcRequest) -> RpcResponse {
        let params = req.params.as_ref().and_then(|p| p.as_array());
        let pubkey_str = match params.and_then(|p| p.first()).and_then(|p| p.as_str()) {
            Some(s) => s,
            None => {
                return RpcResponse::error(
                    req.id.clone(),
                    RpcError::invalid_params("Missing pubkey parameter"),
                );
            }
        };

        let pubkey_bytes = match hex::decode(pubkey_str) {
            Ok(bytes) if bytes.len() == 32 => {
                let mut key = [0u8; 32];
                key.copy_from_slice(&bytes);
                key
            }
            _ => {
                return RpcResponse::error(
                    req.id.clone(),
                    RpcError::invalid_params("Invalid pubkey"),
                );
            }
        };

        let balance = self
            .accounts
            .load(&pubkey_bytes)
            .map(|a| a.lamports)
            .unwrap_or(0);

        RpcResponse::success(req.id.clone(), serde_json::json!({ "lamports": balance }))
    }

    fn get_account_count(&self, req: &RpcRequest) -> RpcResponse {
        let count = self.accounts.account_count();
        RpcResponse::success(req.id.clone(), serde_json::json!(count))
    }

    fn send_transaction(&self, req: &RpcRequest) -> RpcResponse {
        // Stub: would deserialize and validate transaction
        RpcResponse::success(req.id.clone(), serde_json::json!("transaction_received"))
    }

    fn get_transaction(&self, req: &RpcRequest) -> RpcResponse {
        RpcResponse::success(req.id.clone(), Value::Null)
    }

    fn get_block_height(&self, req: &RpcRequest) -> RpcResponse {
        let slot = *self.slot.read();
        RpcResponse::success(req.id.clone(), serde_json::json!(slot))
    }

    fn get_epoch_info(&self, req: &RpcRequest) -> RpcResponse {
        let slot = *self.slot.read();
        RpcResponse::success(
            req.id.clone(),
            serde_json::json!({
                "slot": slot,
                "epoch": slot / 432000,
                "slotIndex": slot % 432000,
                "slotsInEpoch": 432000,
                "absoluteSlot": slot,
                "blockHeight": slot,
                "transactionCount": 0
            }),
        )
    }

    /// Update the current slot (called by the validator)
    pub fn update_slot(&self, slot: u64) {
        *self.slot.write() = slot;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use solana_accounts::account::{random_pubkey, Account};

    #[test]
    fn test_get_health() {
        let db = Arc::new(AccountsDB::new());
        let handler = RpcHandler::new(db, [0u8; 32]);

        let req = RpcRequest {
            jsonrpc: "2.0".to_string(),
            method: "getHealth".to_string(),
            params: None,
            id: Some(serde_json::json!(1)),
        };

        let resp = handler.handle(&req);
        assert!(resp.result.is_some());
        assert_eq!(resp.result.unwrap(), serde_json::json!("ok"));
    }

    #[test]
    fn test_get_version() {
        let db = Arc::new(AccountsDB::new());
        let handler = RpcHandler::new(db, [0u8; 32]);

        let req = RpcRequest {
            jsonrpc: "2.0".to_string(),
            method: "getVersion".to_string(),
            params: None,
            id: Some(serde_json::json!(1)),
        };

        let resp = handler.handle(&req);
        let result = resp.result.unwrap();
        assert_eq!(result["solana-core"], "0.1.0-custom");
    }

    #[test]
    fn test_method_not_found() {
        let db = Arc::new(AccountsDB::new());
        let handler = RpcHandler::new(db, [0u8; 32]);

        let req = RpcRequest {
            jsonrpc: "2.0".to_string(),
            method: "nonExistentMethod".to_string(),
            params: None,
            id: Some(serde_json::json!(1)),
        };

        let resp = handler.handle(&req);
        assert!(resp.error.is_some());
        assert_eq!(resp.error.unwrap().code, -32601);
    }

    #[test]
    fn test_get_account_info() {
        let db = Arc::new(AccountsDB::new());
        let key = random_pubkey();
        let acc = Account::new_system_account(key, 1_000_000);
        db.store(key, &acc);

        let handler = RpcHandler::new(db, [0u8; 32]);

        let req = RpcRequest {
            jsonrpc: "2.0".to_string(),
            method: "getAccountInfo".to_string(),
            params: Some(serde_json::json!([hex::encode(key)])),
            id: Some(serde_json::json!(1)),
        };

        let resp = handler.handle(&req);
        let result = resp.result.unwrap();
        assert_eq!(result["lamports"], 1_000_000);
    }

    #[test]
    fn test_get_balance() {
        let db = Arc::new(AccountsDB::new());
        let key = random_pubkey();
        let acc = Account::new_system_account(key, 500);
        db.store(key, &acc);

        let handler = RpcHandler::new(db, [0u8; 32]);

        let req = RpcRequest {
            jsonrpc: "2.0".to_string(),
            method: "getBalance".to_string(),
            params: Some(serde_json::json!([hex::encode(key)])),
            id: Some(serde_json::json!(1)),
        };

        let resp = handler.handle(&req);
        let result = resp.result.unwrap();
        assert_eq!(result["lamports"], 500);
    }
}
