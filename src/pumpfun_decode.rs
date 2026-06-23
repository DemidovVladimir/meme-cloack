//! Decode pump.fun program events out of a Helius `transactionSubscribe`
//! (jsonParsed) notification.
//!
//! pump.fun emits its `CreateEvent` / `TradeEvent` via Anchor `emit_cpi!`, i.e. as
//! **self-CPI inner instructions** whose data is
//! `[event-CPI sentinel (8)] ++ [event discriminator (8)] ++ borsh(event)`.
//! We walk `meta.innerInstructions`, keep the ones whose program is the pump
//! program, base58-decode the data, match the discriminators, and borsh-decode
//! only the **stable prefix** of each event (reader form tolerates the trailing
//! fields newer program versions append).
//!
//! Market cap is computed from the bonding-curve virtual reserves and matches
//! PumpPortal's `market_cap_sol` (≈ 28 SOL at a fresh launch — see the test).

use borsh::BorshDeserialize;
use serde_json::Value;

use crate::model::{Frame, NewToken, Trade};

pub const PUMP_PROGRAM_ID: &str = "6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P";

/// Anchor event-CPI sentinel (first 8 bytes of every `emit_cpi!` payload).
const EVENT_CPI_DISC: [u8; 8] = [0xe4, 0x45, 0xa5, 0x2e, 0x51, 0xcb, 0x9a, 0x1d];
const CREATE_EVENT_DISC: [u8; 8] = [27, 114, 169, 77, 222, 235, 99, 118];
const TRADE_EVENT_DISC: [u8; 8] = [189, 219, 127, 211, 78, 230, 97, 238];

/// pump.fun fixed total supply in base units (1e9 tokens × 1e6 decimals).
const TOTAL_SUPPLY_BASE: f64 = 1_000_000_000_000_000.0;
const LAMPORTS_PER_SOL: f64 = 1_000_000_000.0;
const TOKEN_DECIMALS: f64 = 1_000_000.0;
/// Fresh-launch virtual reserves (used if a create has no dev buy to read from).
const LAUNCH_VSOL_LAMPORTS: u64 = 30_000_000_000;
const LAUNCH_VTOK_BASE: u64 = 1_073_000_000_000_000;

/// Market cap in SOL from bonding-curve virtual reserves (both in base units).
/// `supply * vSOL / (vTokens * lamports_per_sol)`.
pub fn market_cap_sol(vsol_lamports: u64, vtok_base: u64) -> f64 {
    if vtok_base == 0 {
        return 0.0;
    }
    TOTAL_SUPPLY_BASE * vsol_lamports as f64 / (vtok_base as f64 * LAMPORTS_PER_SOL)
}

#[derive(BorshDeserialize)]
struct CreateEventPrefix {
    name: String,
    symbol: String,
    uri: String,
    mint: [u8; 32],
    #[allow(dead_code)]
    bonding_curve: [u8; 32],
    user: [u8; 32],
}

#[derive(BorshDeserialize)]
struct TradeEventPrefix {
    mint: [u8; 32],
    sol_amount: u64,
    token_amount: u64,
    is_buy: bool,
    user: [u8; 32],
    timestamp: i64,
    virtual_sol_reserves: u64,
    virtual_token_reserves: u64,
}

enum PumpEvent {
    Create {
        mint: String,
        name: String,
        symbol: String,
        uri: String,
        creator: String,
    },
    Trade {
        mint: String,
        is_buy: bool,
        trader: String,
        sol_lamports: u64,
        token_base: u64,
        vsol: u64,
        vtok: u64,
        ts: i64,
    },
}

fn b58(bytes: &[u8; 32]) -> String {
    bs58::encode(bytes).into_string()
}

/// Decode one pump.fun inner-instruction data blob (already base58-decoded).
fn decode_event(bytes: &[u8]) -> Option<PumpEvent> {
    if bytes.len() < 16 || bytes[0..8] != EVENT_CPI_DISC {
        return None;
    }
    let disc = &bytes[8..16];
    let mut payload = &bytes[16..];
    if disc == CREATE_EVENT_DISC {
        let e = CreateEventPrefix::deserialize(&mut payload).ok()?;
        Some(PumpEvent::Create {
            mint: b58(&e.mint),
            name: e.name,
            symbol: e.symbol,
            uri: e.uri,
            creator: b58(&e.user),
        })
    } else if disc == TRADE_EVENT_DISC {
        let e = TradeEventPrefix::deserialize(&mut payload).ok()?;
        Some(PumpEvent::Trade {
            mint: b58(&e.mint),
            is_buy: e.is_buy,
            trader: b58(&e.user),
            sol_lamports: e.sol_amount,
            token_base: e.token_amount,
            vsol: e.virtual_sol_reserves,
            vtok: e.virtual_token_reserves,
            ts: e.timestamp,
        })
    } else {
        None
    }
}

/// Collect resolved account-key strings (jsonParsed gives pubkey strings; if an
/// instruction only carries `programIdIndex` we resolve against this list).
fn account_keys(result: &Value) -> Vec<String> {
    let mut keys: Vec<String> = Vec::new();
    let msg = result
        .pointer("/transaction/transaction/message")
        .or_else(|| result.pointer("/transaction/message"));
    if let Some(arr) = msg.and_then(|m| m.get("accountKeys")).and_then(Value::as_array) {
        for k in arr {
            if let Some(s) = k.as_str() {
                keys.push(s.to_string());
            } else if let Some(s) = k.get("pubkey").and_then(Value::as_str) {
                keys.push(s.to_string());
            }
        }
    }
    let meta = result.pointer("/transaction/meta").or_else(|| result.get("meta"));
    if let Some(meta) = meta {
        for field in ["loadedAddresses/writable", "loadedAddresses/readonly"] {
            if let Some(arr) = meta.pointer(&format!("/{field}")).and_then(Value::as_array) {
                for k in arr {
                    if let Some(s) = k.as_str() {
                        keys.push(s.to_string());
                    }
                }
            }
        }
    }
    keys
}

fn ix_program_id(ix: &Value, keys: &[String]) -> Option<String> {
    if let Some(s) = ix.get("programId").and_then(Value::as_str) {
        return Some(s.to_string());
    }
    let idx = ix.get("programIdIndex").and_then(Value::as_u64)? as usize;
    keys.get(idx).cloned()
}

/// Decode every pump.fun event in one `transactionNotification` result into
/// ordered Frames. A create tx yields the NewToken first, then its dev-buy Trade.
pub fn decode_result(result: &Value, fallback_ms: i64) -> Vec<Frame> {
    let meta = match result.pointer("/transaction/meta").or_else(|| result.get("meta")) {
        Some(m) => m,
        None => return Vec::new(),
    };
    let inner = match meta.get("innerInstructions").and_then(Value::as_array) {
        Some(a) => a,
        None => return Vec::new(),
    };
    let keys = account_keys(result);

    let mut events: Vec<PumpEvent> = Vec::new();
    for group in inner {
        let Some(ixs) = group.get("instructions").and_then(Value::as_array) else {
            continue;
        };
        for ix in ixs {
            if ix_program_id(ix, &keys).as_deref() != Some(PUMP_PROGRAM_ID) {
                continue;
            }
            let Some(data) = ix.get("data").and_then(Value::as_str) else {
                continue;
            };
            let Ok(bytes) = bs58::decode(data).into_vec() else {
                continue;
            };
            if let Some(ev) = decode_event(&bytes) {
                events.push(ev);
            }
        }
    }
    if events.is_empty() {
        return Vec::new();
    }

    let slot = result
        .get("slot")
        .and_then(Value::as_i64)
        .or_else(|| result.pointer("/transaction/slot").and_then(Value::as_i64))
        .unwrap_or(0);
    let sig = result
        .get("signature")
        .and_then(Value::as_str)
        .or_else(|| {
            result
                .pointer("/transaction/transaction/signatures/0")
                .and_then(Value::as_str)
        })
        .unwrap_or("")
        .to_string();
    let raw = serde_json::json!({"src": "helius", "slot": slot, "sig": sig}).to_string();

    // The dev buy (first trade in the tx) carries the create's timestamp, reserves
    // and initial-buy size.
    let first_trade = events.iter().find_map(|e| match e {
        PumpEvent::Trade {
            sol_lamports,
            vsol,
            vtok,
            ts,
            ..
        } => Some((*sol_lamports, *vsol, *vtok, *ts)),
        _ => None,
    });
    let create_ts_ms = first_trade
        .map(|(_, _, _, ts)| ts * 1000)
        .filter(|&t| t > 0)
        .unwrap_or(fallback_ms);

    let mut frames: Vec<Frame> = Vec::new();
    for ev in events {
        match ev {
            PumpEvent::Create {
                mint,
                name,
                symbol,
                uri,
                creator,
            } => {
                let (mcap, vsol, vtok, init_buy) = match first_trade {
                    Some((sol, vsol, vtok, _)) => (
                        market_cap_sol(vsol, vtok),
                        vsol as f64 / LAMPORTS_PER_SOL,
                        vtok as f64 / TOKEN_DECIMALS,
                        sol as f64 / LAMPORTS_PER_SOL,
                    ),
                    None => (
                        market_cap_sol(LAUNCH_VSOL_LAMPORTS, LAUNCH_VTOK_BASE),
                        LAUNCH_VSOL_LAMPORTS as f64 / LAMPORTS_PER_SOL,
                        LAUNCH_VTOK_BASE as f64 / TOKEN_DECIMALS,
                        0.0,
                    ),
                };
                frames.push(Frame::NewToken(NewToken {
                    mint,
                    name: Some(name),
                    symbol: Some(symbol),
                    creator: Some(creator),
                    created_ms: create_ts_ms,
                    pool: Some("pump".to_string()),
                    market_cap_sol: Some(mcap),
                    v_sol_in_curve: Some(vsol),
                    v_tokens_in_curve: Some(vtok),
                    initial_buy_sol: Some(init_buy),
                    uri: Some(uri),
                    signature: Some(sig.clone()),
                    raw_json: raw.clone(),
                }));
            }
            PumpEvent::Trade {
                mint,
                is_buy,
                trader,
                sol_lamports,
                token_base,
                vsol,
                vtok,
                ts,
            } => {
                let ts_ms = if ts > 0 { ts * 1000 } else { fallback_ms };
                frames.push(Frame::Trade(Trade {
                    mint,
                    side: if is_buy { "buy" } else { "sell" }.to_string(),
                    trader,
                    signature: Some(sig.clone()),
                    sol_amount: Some(sol_lamports as f64 / LAMPORTS_PER_SOL),
                    token_amount: Some(token_base as f64 / TOKEN_DECIMALS),
                    market_cap_sol: Some(market_cap_sol(vsol, vtok)),
                    v_sol_in_curve: Some(vsol as f64 / LAMPORTS_PER_SOL),
                    v_tokens_in_curve: Some(vtok as f64 / TOKEN_DECIMALS),
                    pool: Some("pump".to_string()),
                    new_token_balance: None,
                    ts_ms,
                    raw_json: raw.clone(),
                }));
            }
        }
    }
    frames
}

#[cfg(test)]
mod tests {
    use super::*;
    use borsh::BorshSerialize;

    #[derive(BorshSerialize)]
    struct TradeEventWire {
        mint: [u8; 32],
        sol_amount: u64,
        token_amount: u64,
        is_buy: bool,
        user: [u8; 32],
        timestamp: i64,
        virtual_sol_reserves: u64,
        virtual_token_reserves: u64,
        // a trailing field a newer program version might append — must be ignored
        real_sol_reserves: u64,
    }

    fn b58_event(disc: [u8; 8], body: &[u8]) -> String {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&EVENT_CPI_DISC);
        bytes.extend_from_slice(&disc);
        bytes.extend_from_slice(body);
        bs58::encode(bytes).into_string()
    }

    #[test]
    fn market_cap_baseline_is_28_sol() {
        let mc = market_cap_sol(LAUNCH_VSOL_LAMPORTS, LAUNCH_VTOK_BASE);
        assert!((mc - 27.96).abs() < 0.1, "baseline mcap was {mc}");
    }

    #[test]
    fn decodes_trade_event_with_trailing_field() {
        let wire = TradeEventWire {
            mint: [7u8; 32],
            sol_amount: 1_000_000_000, // 1 SOL
            token_amount: 2_000_000,   // 2 tokens (6 decimals)
            is_buy: true,
            user: [9u8; 32],
            timestamp: 1700,
            virtual_sol_reserves: LAUNCH_VSOL_LAMPORTS,
            virtual_token_reserves: LAUNCH_VTOK_BASE,
            real_sol_reserves: 12345, // trailing — reader form must ignore it
        };
        let data = b58_event(TRADE_EVENT_DISC, &borsh::to_vec(&wire).unwrap());
        let result = serde_json::json!({
            "signature": "sig123",
            "slot": 42,
            "transaction": { "meta": { "innerInstructions": [
                { "index": 0, "instructions": [
                    { "programId": PUMP_PROGRAM_ID, "data": data, "accounts": [] }
                ]}
            ]}}
        });
        let frames = decode_result(&result, 999);
        assert_eq!(frames.len(), 1);
        match &frames[0] {
            Frame::Trade(t) => {
                assert_eq!(t.side, "buy");
                assert_eq!(t.mint, bs58::encode([7u8; 32]).into_string());
                assert_eq!(t.trader, bs58::encode([9u8; 32]).into_string());
                assert_eq!(t.sol_amount, Some(1.0));
                assert_eq!(t.token_amount, Some(2.0));
                assert_eq!(t.ts_ms, 1_700_000);
                assert!((t.market_cap_sol.unwrap() - 27.96).abs() < 0.1);
            }
            _ => panic!("expected Trade"),
        }
    }

    #[test]
    fn ignores_non_pump_and_garbage() {
        let result = serde_json::json!({
            "transaction": { "meta": { "innerInstructions": [
                { "instructions": [
                    { "programId": "SomeOtherProgram1111111111111111111111111", "data": "abc" }
                ]}
            ]}}
        });
        assert!(decode_result(&result, 1).is_empty());
    }
}
