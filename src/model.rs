//! PumpPortal websocket frame parsing.
//!
//! Frames are JSON. We branch on `txType`/shape: a "create" is a new token, a
//! "buy"/"sell" is a trade, anything with a `message`/`errors` field is a
//! control frame (e.g. "Successfully subscribed", "Minimum balance not met...").

use serde_json::Value;

#[derive(Debug, Clone)]
pub struct NewToken {
    pub mint: String,
    pub name: Option<String>,
    pub symbol: Option<String>,
    pub creator: Option<String>,
    pub created_ms: i64,
    pub pool: Option<String>,
    pub market_cap_sol: Option<f64>,
    pub v_sol_in_curve: Option<f64>,
    pub v_tokens_in_curve: Option<f64>,
    pub initial_buy_sol: Option<f64>,
    pub uri: Option<String>,
    pub signature: Option<String>,
    pub raw_json: String,
}

#[derive(Debug, Clone)]
pub struct Trade {
    pub mint: String,
    pub side: String,
    pub trader: String,
    pub signature: Option<String>,
    pub sol_amount: Option<f64>,
    pub token_amount: Option<f64>,
    pub market_cap_sol: Option<f64>,
    pub v_sol_in_curve: Option<f64>,
    pub v_tokens_in_curve: Option<f64>,
    pub pool: Option<String>,
    pub new_token_balance: Option<f64>,
    pub ts_ms: i64,
    pub raw_json: String,
}

#[derive(Debug)]
pub enum Frame {
    NewToken(NewToken),
    Trade(Trade),
    /// Control/status/error message from PumpPortal.
    Control { message: String, is_error: bool },
    /// Parseable JSON we don't recognize.
    Unknown,
}

pub fn parse_frame(text: &str, now_ms: i64) -> Frame {
    let value: Value = match serde_json::from_str(text) {
        Ok(v) => v,
        Err(_) => return Frame::Unknown,
    };
    let obj = match value.as_object() {
        Some(o) => o,
        None => return Frame::Unknown,
    };

    let tx_type = obj
        .get("txType")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_ascii_lowercase();

    if tx_type == "create" || (obj.contains_key("mint") && (obj.contains_key("name") || obj.contains_key("symbol"))) {
        if let Some(mint) = first_str(obj, &["mint", "tokenMint", "ca", "address"]) {
            return Frame::NewToken(NewToken {
                mint,
                name: first_str(obj, &["name", "tokenName"]),
                symbol: first_str(obj, &["symbol", "ticker"]),
                creator: first_str(obj, &["traderPublicKey", "creator"]),
                created_ms: now_ms,
                pool: first_str(obj, &["pool"]),
                market_cap_sol: first_f64(obj, &["marketCapSol", "market_cap_sol"]),
                v_sol_in_curve: first_f64(obj, &["vSolInBondingCurve", "virtualSolReserves"]),
                v_tokens_in_curve: first_f64(obj, &["vTokensInBondingCurve", "virtualTokenReserves"]),
                initial_buy_sol: first_f64(obj, &["solAmount", "sol_amount"]),
                uri: first_str(obj, &["uri", "metadataUri"]),
                signature: first_str(obj, &["signature", "txSignature"]),
                raw_json: text.to_string(),
            });
        }
    }

    if tx_type == "buy" || tx_type == "sell" {
        if let Some(mint) = first_str(obj, &["mint", "tokenMint", "ca", "address"]) {
            return Frame::Trade(Trade {
                mint,
                side: tx_type,
                trader: first_str(obj, &["traderPublicKey", "wallet"]).unwrap_or_default(),
                signature: first_str(obj, &["signature", "txSignature"]),
                sol_amount: first_f64(obj, &["solAmount", "sol_amount"]),
                token_amount: first_f64(obj, &["tokenAmount", "token_amount"]),
                market_cap_sol: first_f64(obj, &["marketCapSol", "market_cap_sol"]),
                v_sol_in_curve: first_f64(obj, &["vSolInBondingCurve", "virtualSolReserves"]),
                v_tokens_in_curve: first_f64(obj, &["vTokensInBondingCurve", "virtualTokenReserves"]),
                pool: first_str(obj, &["pool"]),
                new_token_balance: first_f64(obj, &["newTokenBalance"]),
                ts_ms: now_ms,
                raw_json: text.to_string(),
            });
        }
    }

    if let Some(message) = first_str(obj, &["message", "error", "errors", "status"]) {
        let is_error = obj.contains_key("error")
            || obj.contains_key("errors")
            || message.to_ascii_lowercase().contains("minimum balance")
            || message.to_ascii_lowercase().contains("error");
        return Frame::Control { message, is_error };
    }

    Frame::Unknown
}

fn first_str(obj: &serde_json::Map<String, Value>, keys: &[&str]) -> Option<String> {
    for key in keys {
        match obj.get(*key) {
            Some(Value::String(s)) if !s.is_empty() => return Some(s.clone()),
            Some(v @ Value::Number(_)) => return Some(v.to_string()),
            _ => {}
        }
    }
    None
}

fn first_f64(obj: &serde_json::Map<String, Value>, keys: &[&str]) -> Option<f64> {
    for key in keys {
        match obj.get(*key) {
            Some(Value::Number(n)) => return n.as_f64(),
            Some(Value::String(s)) => {
                if let Ok(f) = s.parse::<f64>() {
                    return Some(f);
                }
            }
            _ => {}
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_new_token() {
        let f = parse_frame(
            r#"{"txType":"create","mint":"ABC","name":"Doge","symbol":"DOGE","traderPublicKey":"W1","marketCapSol":30.5,"solAmount":1.2}"#,
            1000,
        );
        match f {
            Frame::NewToken(t) => {
                assert_eq!(t.mint, "ABC");
                assert_eq!(t.symbol.as_deref(), Some("DOGE"));
                assert_eq!(t.creator.as_deref(), Some("W1"));
                assert_eq!(t.market_cap_sol, Some(30.5));
                assert_eq!(t.created_ms, 1000);
            }
            _ => panic!("expected NewToken"),
        }
    }

    #[test]
    fn parses_trade() {
        let f = parse_frame(
            r#"{"txType":"buy","mint":"ABC","traderPublicKey":"W2","solAmount":0.5,"marketCapSol":40.0}"#,
            2000,
        );
        match f {
            Frame::Trade(t) => {
                assert_eq!(t.side, "buy");
                assert_eq!(t.trader, "W2");
                assert_eq!(t.market_cap_sol, Some(40.0));
            }
            _ => panic!("expected Trade"),
        }
    }

    #[test]
    fn parses_control_error() {
        let f = parse_frame(r#"{"message":"Minimum balance not met for PumpSwap websocket data."}"#, 3000);
        match f {
            Frame::Control { is_error, .. } => assert!(is_error),
            _ => panic!("expected Control"),
        }
    }
}
