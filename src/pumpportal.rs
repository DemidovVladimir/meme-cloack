//! Reconnecting PumpPortal websocket client.
//!
//! One connection, supervised with exponential backoff. Subscriptions live in
//! `SubState` (the source of truth) so they are replayed on every reconnect.
//! Outgoing subscribe/unsubscribe frames flow through an unbounded channel; on
//! reconnect we drain stale queued frames and resubscribe from `SubState` to
//! avoid double-billing the metered trade stream.

use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use tokio::sync::{mpsc, watch};
use tokio::time::{sleep, Instant};
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::Message;
use tracing::{info, warn};

use crate::model::{parse_frame, Frame};
use crate::util::now_ms;

#[derive(Default)]
pub struct SubState {
    pub new_token: bool,
    pub trade_keys: HashSet<String>,
}

/// Handle used by the ingest coordinator to manage subscriptions. Cloneable.
#[derive(Clone)]
pub struct Controller {
    out_tx: mpsc::UnboundedSender<String>,
    subs: Arc<Mutex<SubState>>,
}

impl Controller {
    pub fn subscribe_new_token(&self) {
        self.subs.lock().unwrap().new_token = true;
        let _ = self.out_tx.send(sub_new_token());
    }

    pub fn subscribe_trades(&self, mints: &[String]) {
        if mints.is_empty() {
            return;
        }
        {
            let mut s = self.subs.lock().unwrap();
            for m in mints {
                s.trade_keys.insert(m.clone());
            }
        }
        let _ = self.out_tx.send(sub_trades(mints));
    }

    /// Drop mints from the subscription set. `send_frame=false` only updates the
    /// authoritative set (used when offline — the actual frame can't be sent).
    pub fn unsubscribe_trades(&self, mints: &[String], send_frame: bool) {
        if mints.is_empty() {
            return;
        }
        {
            let mut s = self.subs.lock().unwrap();
            for m in mints {
                s.trade_keys.remove(m);
            }
        }
        if send_frame {
            let _ = self.out_tx.send(unsub_trades(mints));
        }
    }

    pub fn active_trade_subs(&self) -> usize {
        self.subs.lock().unwrap().trade_keys.len()
    }
}

/// Create a controller plus the supervisor future. Drive the future on a task.
pub fn start(
    url: String,
    frame_tx: mpsc::UnboundedSender<Frame>,
    shutdown_rx: watch::Receiver<bool>,
) -> (Controller, impl std::future::Future<Output = ()>) {
    let (out_tx, out_rx) = mpsc::unbounded_channel::<String>();
    let subs = Arc::new(Mutex::new(SubState::default()));
    let controller = Controller { out_tx, subs: subs.clone() };
    let fut = supervise(url, subs, out_rx, frame_tx, shutdown_rx);
    (controller, fut)
}

async fn supervise(
    url: String,
    subs: Arc<Mutex<SubState>>,
    mut out_rx: mpsc::UnboundedReceiver<String>,
    frame_tx: mpsc::UnboundedSender<Frame>,
    mut shutdown_rx: watch::Receiver<bool>,
) {
    let mut backoff_ms: u64 = 500;
    loop {
        if *shutdown_rx.borrow() {
            return;
        }
        let started = Instant::now();
        match run_connection(&url, &subs, &mut out_rx, &frame_tx, &mut shutdown_rx).await {
            ConnOutcome::Shutdown => return,
            ConnOutcome::Disconnected(reason) => {
                if started.elapsed() > Duration::from_secs(30) {
                    backoff_ms = 500;
                }
                warn!(reason, backoff_ms, "pumpportal disconnected; reconnecting");
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

async fn run_connection(
    url: &str,
    subs: &Arc<Mutex<SubState>>,
    out_rx: &mut mpsc::UnboundedReceiver<String>,
    frame_tx: &mpsc::UnboundedSender<Frame>,
    shutdown_rx: &mut watch::Receiver<bool>,
) -> ConnOutcome {
    let (ws, _resp) = match connect_async(url).await {
        Ok(ok) => ok,
        Err(e) => {
            warn!("connect failed: {e}");
            return ConnOutcome::Disconnected("connect error");
        }
    };
    info!("pumpportal connected");
    let (mut write, mut read) = ws.split();

    // Drop any stale queued frames, then resubscribe from the source of truth.
    while out_rx.try_recv().is_ok() {}
    for frame in resubscribe_frames(subs) {
        if write.send(Message::text(frame)).await.is_err() {
            return ConnOutcome::Disconnected("send on connect failed");
        }
    }

    let mut ping = tokio::time::interval(Duration::from_secs(20));
    loop {
        tokio::select! {
            incoming = read.next() => match incoming {
                Some(Ok(Message::Text(txt))) => {
                    let _ = frame_tx.send(parse_frame(txt.as_str(), now_ms()));
                }
                Some(Ok(Message::Binary(bin))) => {
                    if let Ok(s) = std::str::from_utf8(&bin) {
                        let _ = frame_tx.send(parse_frame(s, now_ms()));
                    }
                }
                Some(Ok(Message::Ping(p))) => {
                    let _ = write.send(Message::Pong(p)).await;
                }
                Some(Ok(_)) => {}
                Some(Err(e)) => {
                    warn!("read error: {e}");
                    return ConnOutcome::Disconnected("read error");
                }
                None => return ConnOutcome::Disconnected("stream closed"),
            },
            out = out_rx.recv() => match out {
                Some(frame) => {
                    if write.send(Message::text(frame)).await.is_err() {
                        return ConnOutcome::Disconnected("send failed");
                    }
                }
                None => return ConnOutcome::Shutdown, // controller dropped
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

fn resubscribe_frames(subs: &Arc<Mutex<SubState>>) -> Vec<String> {
    let s = subs.lock().unwrap();
    let mut frames = Vec::new();
    if s.new_token {
        frames.push(sub_new_token());
    }
    if !s.trade_keys.is_empty() {
        let keys: Vec<String> = s.trade_keys.iter().cloned().collect();
        frames.push(sub_trades(&keys));
    }
    frames
}

fn sub_new_token() -> String {
    r#"{"method":"subscribeNewToken"}"#.to_string()
}

fn sub_trades(keys: &[String]) -> String {
    method_with_keys("subscribeTokenTrade", keys)
}

fn unsub_trades(keys: &[String]) -> String {
    method_with_keys("unsubscribeTokenTrade", keys)
}

fn method_with_keys(method: &str, keys: &[String]) -> String {
    serde_json::json!({ "method": method, "keys": keys }).to_string()
}
