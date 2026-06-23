//! Reconnecting Helius LaserStream **WebSocket** client.
//!
//! One static subscription: `transactionSubscribe` filtered to the pump.fun
//! program (`accountInclude`). That single stream carries every pump.fun token
//! creation + trade — no per-token subscriptions, no 500-sub cap. Incoming
//! notifications are decoded by [`crate::pumpfun_decode`] into [`Frame`]s.
//!
//! Supervised with exponential backoff (mirrors `pumpportal.rs`); the
//! subscription is simply re-sent on every reconnect.

use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use serde_json::Value;
use tokio::sync::{mpsc, watch};
use tokio::time::{sleep, Instant};
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::Message;
use tracing::{info, warn};

use crate::model::Frame;
use crate::pumpfun_decode;
use crate::util::now_ms;

/// Create the supervisor future. Drive it on a task.
pub fn start(
    url: String,
    program_id: String,
    frame_tx: mpsc::UnboundedSender<Frame>,
    shutdown_rx: watch::Receiver<bool>,
) -> impl std::future::Future<Output = ()> {
    supervise(url, program_id, frame_tx, shutdown_rx)
}

async fn supervise(
    url: String,
    program_id: String,
    frame_tx: mpsc::UnboundedSender<Frame>,
    mut shutdown_rx: watch::Receiver<bool>,
) {
    let mut backoff_ms: u64 = 500;
    loop {
        if *shutdown_rx.borrow() {
            return;
        }
        let started = Instant::now();
        match run_connection(&url, &program_id, &frame_tx, &mut shutdown_rx).await {
            ConnOutcome::Shutdown => return,
            ConnOutcome::Disconnected(reason) => {
                if started.elapsed() > Duration::from_secs(30) {
                    backoff_ms = 500;
                }
                warn!(reason, backoff_ms, "helius ws disconnected; reconnecting");
                sleep(Duration::from_millis(backoff_ms)).await;
                backoff_ms = (backoff_ms * 2).min(60_000);
            }
        }
    }
}

enum ConnOutcome {
    Shutdown,
    Disconnected(&'static str),
}

fn subscribe_request(program_id: &str) -> String {
    serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "transactionSubscribe",
        "params": [
            { "accountInclude": [program_id], "failed": false, "vote": false },
            {
                "commitment": "confirmed",
                "encoding": "jsonParsed",
                "transactionDetails": "full",
                "maxSupportedTransactionVersion": 0
            }
        ]
    })
    .to_string()
}

async fn run_connection(
    url: &str,
    program_id: &str,
    frame_tx: &mpsc::UnboundedSender<Frame>,
    shutdown_rx: &mut watch::Receiver<bool>,
) -> ConnOutcome {
    let (ws, _resp) = match connect_async(url).await {
        Ok(ok) => ok,
        Err(e) => {
            warn!("helius connect failed: {e}");
            return ConnOutcome::Disconnected("connect error");
        }
    };
    let (mut write, mut read) = ws.split();
    if write
        .send(Message::text(subscribe_request(program_id)))
        .await
        .is_err()
    {
        return ConnOutcome::Disconnected("subscribe send failed");
    }
    info!(program = program_id, "helius ws connected; subscribed to pump.fun transactions");

    let mut ping = tokio::time::interval(Duration::from_secs(20));
    loop {
        tokio::select! {
            incoming = read.next() => match incoming {
                Some(Ok(Message::Text(txt))) => handle_text(txt.as_str(), frame_tx),
                Some(Ok(Message::Binary(bin))) => {
                    if let Ok(s) = std::str::from_utf8(&bin) {
                        handle_text(s, frame_tx);
                    }
                }
                Some(Ok(Message::Ping(p))) => { let _ = write.send(Message::Pong(p)).await; }
                Some(Ok(_)) => {}
                Some(Err(e)) => {
                    warn!("helius read error: {e}");
                    return ConnOutcome::Disconnected("read error");
                }
                None => return ConnOutcome::Disconnected("stream closed"),
            },
            _ = ping.tick() => {
                let _ = write.send(Message::Ping(Vec::new().into())).await;
            }
            _ = shutdown_rx.changed() => {
                if *shutdown_rx.borrow() {
                    let _ = write.send(Message::Close(None)).await;
                    return ConnOutcome::Shutdown;
                }
            }
        }
    }
}

fn handle_text(txt: &str, frame_tx: &mpsc::UnboundedSender<Frame>) {
    let value: Value = match serde_json::from_str(txt) {
        Ok(v) => v,
        Err(_) => return,
    };
    // Subscription ack: {"jsonrpc":"2.0","result":<id>,"id":1} — ignore.
    if let Some(err) = value.get("error") {
        warn!(%err, "helius ws error frame");
        return;
    }
    if value.get("method").and_then(Value::as_str) != Some("transactionNotification") {
        return;
    }
    let Some(result) = value.pointer("/params/result") else {
        return;
    };
    for frame in pumpfun_decode::decode_result(result, now_ms()) {
        let _ = frame_tx.send(frame);
    }
}
