use std::collections::HashSet;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use base64::Engine;
use serde::Serialize;
use stellar_xdr::{
    ContractDataDurability, LedgerEntryData, LedgerKey, LedgerKeyContractData, Limits, ReadXdr,
    ScVal, WriteXdr,
};

use crate::config::Config;
use crate::keeper::KeeperBalanceConfig;

const ORDER_KEEPER_ROLE: &str = "ORDER_KEEPER";

#[derive(Debug, Clone, Serialize)]
pub struct PreflightCheck {
    pub name: &'static str,
    pub ok: bool,
    pub hard: bool,
    pub detail: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct PreflightResult {
    pub checks: Vec<PreflightCheck>,
}

impl PreflightResult {
    pub fn hard_failed(&self) -> bool {
        self.checks.iter().any(|check| check.hard && !check.ok)
    }
}

pub async fn preflight(config: &Config) -> PreflightResult {
    let checks = vec![
        check_keeper_key(config),
        check_keeper_balance(config).await,
        check_order_keeper_role(config).await,
        check_contracts_deployed(config).await,
        check_token_contracts_deployed(config).await,
    ];

    for check in &checks {
        if check.ok {
            tracing::info!(check = check.name, detail = %check.detail, "preflight check passed");
        } else {
            tracing::error!(
                check = check.name,
                hard = check.hard,
                detail = %check.detail,
                "preflight check failed"
            );
        }
    }

    PreflightResult { checks }
}

fn check_keeper_key(config: &Config) -> PreflightCheck {
    const NAME: &str = "keeper_account_matches_secret_key";
    let derived = derive_keeper_account_id(config.keeper_secret_key.as_str());
    match derived {
        Ok(account_id) if account_id == config.keeper_account_id => PreflightCheck {
            name: NAME,
            ok: true,
            hard: true,
            detail: format!("KEEPER_ACCOUNT_ID matches derived key {account_id}"),
        },
        Ok(account_id) => PreflightCheck {
            name: NAME,
            ok: false,
            hard: true,
            detail: format!(
                "KEEPER_ACCOUNT_ID {} does not match KEEPER_SECRET_KEY-derived account {}",
                config.keeper_account_id, account_id
            ),
        },
        Err(error) => PreflightCheck {
            name: NAME,
            ok: false,
            hard: true,
            detail: error,
        },
    }
}

fn derive_keeper_account_id(secret_key: &str) -> Result<String, String> {
    let seed = stellar_strkey::ed25519::PrivateKey::from_string(secret_key)
        .map_err(|e| format!("invalid KEEPER_SECRET_KEY strkey: {e}"))?
        .0
        .to_vec();
    if seed.len() != 32 {
        return Err(format!(
            "KEEPER_SECRET_KEY must be 32 bytes, got {}",
            seed.len()
        ));
    }
    let mut key_array = [0u8; 32];
    key_array.copy_from_slice(&seed);
    let signing_key = ed25519_dalek::SigningKey::from_bytes(&key_array);
    Ok(stellar_strkey::ed25519::PublicKey(
        signing_key.verifying_key().to_bytes(),
    )
    .to_string())
}

async fn check_keeper_balance(config: &Config) -> PreflightCheck {
    const NAME: &str = "keeper_balance";
    let balance_config = KeeperBalanceConfig {
        horizon_url: config.horizon_url.clone(),
        account_id: config.keeper_account_id.clone(),
        min_balance_xlm: config.min_keeper_balance_xlm,
    };
    let below_min = Arc::new(AtomicBool::new(false));
    match crate::keeper::check_keeper_balance(&balance_config, &below_min).await {
        Ok(balance) => PreflightCheck {
            name: NAME,
            ok: true,
            hard: false,
            detail: format!(
                "keeper spendable balance {} stroops is at or above minimum {} XLM",
                balance.spendable_stroops, config.min_keeper_balance_xlm
            ),
        },
        Err(error) => PreflightCheck {
            name: NAME,
            ok: false,
            hard: false,
            detail: error.to_string(),
        },
    }
}

async fn check_order_keeper_role(config: &Config) -> PreflightCheck {
    const NAME: &str = "keeper_holds_order_keeper_role";
    let result = simulate_call(
        config,
        &config.role_store_contract_id,
        "has_role",
        &[ORDER_KEEPER_ROLE, config.keeper_account_id.as_str()],
    )
    .await;
    match result {
        Ok(raw) => match parse_bool_from_result(&raw) {
            Ok(true) => PreflightCheck {
                name: NAME,
                ok: true,
                hard: true,
                detail: format!(
                    "keeper {} holds {ORDER_KEEPER_ROLE} in role store {}",
                    config.keeper_account_id, config.role_store_contract_id
                ),
            },
            Ok(false) => PreflightCheck {
                name: NAME,
                ok: false,
                hard: true,
                detail: format!(
                    "keeper {} does not hold {ORDER_KEEPER_ROLE} in role store {}",
                    config.keeper_account_id, config.role_store_contract_id
                ),
            },
            Err(error) => PreflightCheck {
                name: NAME,
                ok: false,
                hard: true,
                detail: error,
            },
        },
        Err(error) => PreflightCheck {
            name: NAME,
            ok: false,
            hard: true,
            detail: error,
        },
    }
}

async fn check_contracts_deployed(config: &Config) -> PreflightCheck {
    const NAME: &str = "contracts_deployed";
    let addresses = vec![
        config.oracle_contract_id.clone(),
        config.role_store_contract_id.clone(),
        config.data_store_contract_id.clone(),
        config.order_handler_contract_id.clone(),
        config.deposit_handler_contract_id.clone(),
        config.withdrawal_handler_contract_id.clone(),
        config.reader_contract_id.clone(),
    ];
    report_missing(NAME, true, "contract", addresses, config).await
}

async fn check_token_contracts_deployed(config: &Config) -> PreflightCheck {
    const NAME: &str = "token_contracts_deployed";
    let addresses: Vec<String> = config
        .price_feed
        .tokens
        .iter()
        .map(|token| token.stellar_address.clone())
        .collect();
    report_missing(NAME, false, "token contract", addresses, config).await
}

async fn report_missing(
    name: &'static str,
    hard: bool,
    label: &str,
    addresses: Vec<String>,
    config: &Config,
) -> PreflightCheck {
    match missing_contracts(config, &addresses).await {
        Ok(missing) if missing.is_empty() => PreflightCheck {
            name,
            ok: true,
            hard,
            detail: format!("all {} configured {label}(s) are deployed", addresses.len()),
        },
        Ok(missing) => PreflightCheck {
            name,
            ok: false,
            hard,
            detail: format!("{label}(s) not deployed: {}", missing.join(", ")),
        },
        Err(error) => PreflightCheck {
            name,
            ok: false,
            hard,
            detail: error,
        },
    }
}

async fn missing_contracts(config: &Config, addresses: &[String]) -> Result<Vec<String>, String> {
    if addresses.is_empty() {
        return Ok(Vec::new());
    }

    let mut keys = Vec::new();
    let mut hashes = Vec::new();
    for address in addresses {
        let contract = crate::chain::scval::strkey_to_sc_address(address)?;
        let hash = match &contract {
            stellar_xdr::ScAddress::Contract(stellar_xdr::ContractId(stellar_xdr::Hash(bytes))) => {
                *bytes
            }
            other => return Err(format!("not a contract address ({other:?}): {address}")),
        };
        let ledger_key = LedgerKey::ContractData(LedgerKeyContractData {
            contract,
            key: ScVal::LedgerKeyContractInstance,
            durability: ContractDataDurability::Persistent,
        });
        let xdr = ledger_key
            .to_xdr(Limits::none())
            .map_err(|e| format!("failed to encode ledger key for {address}: {e}"))?;
        keys.push(base64::engine::general_purpose::STANDARD.encode(xdr));
        hashes.push((hash, address.clone()));
    }

    let payload = serde_json::to_string(&serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "getLedgerEntries",
        "params": { "keys": keys }
    }))
    .map_err(|e| format!("failed to serialize request: {e}"))?;

    let body = crate::stellar_rpc::rpc_post(&config.stellar_rpc_url, payload)
        .await
        .map_err(|e| e.to_string())?;
    let response_json: serde_json::Value =
        serde_json::from_str(&body).map_err(|e| format!("Failed to parse RPC response: {e}"))?;
    if let Some(error) = response_json.get("error") {
        return Err(format!("getLedgerEntries error: {error}"));
    }

    let mut found = HashSet::new();
    if let Some(entries) = response_json["result"]["entries"].as_array() {
        for entry in entries {
            let xdr_b64 = entry["xdr"]
                .as_str()
                .ok_or("getLedgerEntries entry missing xdr")?;
            let raw = base64::engine::general_purpose::STANDARD
                .decode(xdr_b64)
                .map_err(|e| format!("invalid entry xdr base64: {e}"))?;
            let data = LedgerEntryData::from_xdr(&raw, Limits::none())
                .map_err(|e| format!("failed to decode ledger entry: {e}"))?;
            if let LedgerEntryData::ContractData(entry) = data {
                if let stellar_xdr::ScAddress::Contract(stellar_xdr::ContractId(
                    stellar_xdr::Hash(bytes),
                )) = entry.contract
                {
                    found.insert(bytes);
                }
            }
        }
    }

    Ok(hashes
        .into_iter()
        .filter(|(hash, _)| !found.contains(hash))
        .map(|(_, address)| address)
        .collect())
}

async fn simulate_call(
    config: &Config,
    contract_id: &str,
    method: &str,
    args: &[&str],
) -> Result<String, String> {
    let args_json: Vec<serde_json::Value> = args
        .iter()
        .map(|arg| serde_json::Value::String(arg.to_string()))
        .collect();

    let payload = serde_json::to_string(&serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "simulateTransaction",
        "params": {
            "transaction": {
                "source_account": config.keeper_account_id,
                "fee": "100",
                "network_passphrase": config.network_passphrase,
                "operations": [{
                    "type": "invoke",
                    "contract_id": contract_id,
                    "method": method,
                    "args": args_json
                }]
            }
        }
    }))
    .map_err(|e| format!("failed to serialize request: {e}"))?;

    let body = crate::stellar_rpc::rpc_post(&config.stellar_rpc_url, payload)
        .await
        .map_err(|e| e.to_string())?;

    let response_json: serde_json::Value =
        serde_json::from_str(&body).map_err(|e| format!("Failed to parse RPC response: {e}"))?;

    if let Some(error) = response_json.get("error") {
        return Err(format!("Simulation error: {error}"));
    }

    response_json
        .get("result")
        .map(|result| result.to_string())
        .ok_or_else(|| "Missing result in simulation response".to_string())
}

fn parse_bool_from_result(result: &str) -> Result<bool, String> {
    let value: serde_json::Value =
        serde_json::from_str(result).map_err(|e| format!("failed to parse result: {e}"))?;
    value
        .as_bool()
        .or_else(|| value.get("bool").and_then(|b| b.as_bool()))
        .ok_or_else(|| format!("expected bool value, got: {value}"))
}
