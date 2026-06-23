//! Ingest coordinator: wires the websocket to the writer and runs the per-token
//! 40-minute survivor state machine.
//!
//! Policy (configurable): subscribe to every new token's trades on launch. While
//! a token is younger than `survivor_age_minutes`, drop it the moment it looks
//! dead — market cap collapsed below `death_drawdown_pct` of its running peak, or
//! no trades for `death_silence_minutes`. Tokens that reach the survivor age are
//! kept (and tracked) through the retention window. This bounds PumpPortal spend
//! by *survivor* count, not *launch* count, and means only survivors surface.

use std::collections::{HashMap, HashSet, VecDeque};

use anyhow::Result;
use tokio::sync::{mpsc, watch};
use tokio::time::{interval, Duration};
use tracing::{info, warn};

use crate::config::{Config, IngestSource};
use crate::model::Frame;
use crate::util::now_ms;
use crate::writer::{spawn_writer, WriteMsg};
use crate::{db, helius, model, pumpportal};

struct TokenState {
    created_ms: i64,
    peak_mcap: f64,
    last_mcap: f64,
    last_trade_ms: i64,
    subscribed: bool,
    survived: bool,
}

pub async fn run(config: Config) -> Result<()> {
    match config.ingest_source {
        IngestSource::PumpPortal => run_pumpportal(config).await,
        IngestSource::HeliusWs => run_helius(config).await,
    }
}

/// PumpPortal path: per-token trade subscriptions + 40-min survivor sweep (fallback).
async fn run_pumpportal(config: Config) -> Result<()> {
    let conn = db::open_writer(&config.db_path)?;
    let (write_tx, write_rx) = mpsc::unbounded_channel::<WriteMsg>();
    let writer = spawn_writer(conn, write_rx);

    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let (frame_tx, mut frame_rx) = mpsc::unbounded_channel::<Frame>();
    let (controller, supervisor) =
        pumpportal::start(config.pumpportal_ws_url.clone(), frame_tx, shutdown_rx.clone());
    let supervisor_handle = tokio::spawn(supervisor);

    controller.subscribe_new_token();
    if config.pumpportal_api_key.is_none() {
        warn!("PUMPPORTAL_API_KEY not set — only the free new-token stream works; trades will be empty");
    }
    info!(
        survivor_min = config.survivor_age_minutes,
        drawdown = config.death_drawdown_pct,
        silence_min = config.death_silence_minutes,
        max_subs = config.max_active_trade_subs,
        "ingest started"
    );

    install_shutdown(shutdown_tx.clone());

    let mut states: HashMap<String, TokenState> = HashMap::new();
    let mut buffer: Vec<model::Trade> = Vec::with_capacity(config.writer_flush_rows);
    let mut tokens_seen: u64 = 0;
    let mut trades_seen: u64 = 0;

    let mut flush = interval(Duration::from_millis(config.writer_flush_ms.max(50)));
    let mut sweep = interval(Duration::from_secs_f64((config.prune_interval_minutes * 60.0).max(5.0)));
    let mut snapshot = interval(Duration::from_secs_f64((config.snapshot_interval_minutes * 60.0).max(5.0)));
    let mut shutdown_rx2 = shutdown_rx.clone();

    loop {
        tokio::select! {
            Some(frame) = frame_rx.recv() => {
                match frame {
                    Frame::NewToken(t) => {
                        tokens_seen += 1;
                        let mc = t.market_cap_sol.unwrap_or(0.0);
                        let now = t.created_ms;
                        let _ = write_tx.send(WriteMsg::NewToken(t.clone()));
                        let mut st = TokenState {
                            created_ms: now,
                            peak_mcap: mc,
                            last_mcap: mc,
                            last_trade_ms: now,
                            subscribed: false,
                            survived: false,
                        };
                        if controller.active_trade_subs() < config.max_active_trade_subs {
                            controller.subscribe_trades(std::slice::from_ref(&t.mint));
                            st.subscribed = true;
                            // Only track tokens we actually subscribed to. At capacity the
                            // token is still persisted to the DB, but no trades will arrive
                            // for it (no re-subscription path), so holding state is dead
                            // weight that would also inflate the survivor count.
                            states.insert(t.mint, st);
                        }
                    }
                    Frame::Trade(t) => {
                        trades_seen += 1;
                        if let Some(st) = states.get_mut(&t.mint) {
                            st.last_trade_ms = t.ts_ms;
                            if let Some(mc) = t.market_cap_sol {
                                st.last_mcap = mc;
                                if mc > st.peak_mcap { st.peak_mcap = mc; }
                            }
                        }
                        buffer.push(t);
                        if buffer.len() >= config.writer_flush_rows {
                            let _ = write_tx.send(WriteMsg::Trades(std::mem::take(&mut buffer)));
                        }
                    }
                    Frame::Control { message, is_error } => {
                        if is_error { warn!(%message, "pumpportal control error"); }
                        else { info!(%message, "pumpportal control"); }
                    }
                    Frame::Unknown => {}
                }
            }
            _ = flush.tick() => {
                if !buffer.is_empty() {
                    let _ = write_tx.send(WriteMsg::Trades(std::mem::take(&mut buffer)));
                }
            }
            _ = sweep.tick() => {
                let (dropped, survivors, active) =
                    sweep_states(&mut states, &controller, &config);
                let _ = write_tx.send(WriteMsg::Prune { cutoff_ms: now_ms() - config.retention_ms() });
                info!(
                    tracked = states.len(), active_subs = active, dropped_dead = dropped,
                    survivors, tokens_seen, trades_seen, "sweep"
                );
            }
            _ = snapshot.tick() => {
                let _ = write_tx.send(WriteMsg::Snapshot { path: config.snapshot_path.clone() });
            }
            _ = shutdown_rx2.changed() => {
                if *shutdown_rx2.borrow() { break; }
            }
        }
    }

    info!("shutting down ingest");
    if !buffer.is_empty() {
        let _ = write_tx.send(WriteMsg::Trades(std::mem::take(&mut buffer)));
    }
    // final snapshot so the MCP reader has fresh data, then stop the writer.
    let _ = write_tx.send(WriteMsg::Snapshot { path: config.snapshot_path.clone() });
    let _ = write_tx.send(WriteMsg::Shutdown);
    let _ = tokio::task::spawn_blocking(move || writer.join().ok()).await;
    supervisor_handle.abort();
    Ok(())
}

/// Evaluate every tracked token: drop pre-survivor deaths (unsubscribe), promote
/// survivors, and evict retention-expired tokens. Returns (dropped, survivors, active_subs).
fn sweep_states(
    states: &mut HashMap<String, TokenState>,
    controller: &pumpportal::Controller,
    config: &Config,
) -> (usize, usize, usize) {
    let now = now_ms();
    let retention_ms = config.retention_ms();
    let survivor_ms = config.survivor_age_ms();
    let silence_ms = config.death_silence_ms();

    let mut unsub: Vec<String> = Vec::new();
    let mut remove: Vec<String> = Vec::new();
    let mut new_survivors = 0usize;

    for (mint, st) in states.iter_mut() {
        let age = now - st.created_ms;
        if age >= retention_ms {
            if st.subscribed {
                unsub.push(mint.clone());
            }
            remove.push(mint.clone());
            continue;
        }
        if st.subscribed && !st.survived && age < survivor_ms {
            let collapsed = st.peak_mcap > 0.0 && st.last_mcap < config.death_drawdown_pct * st.peak_mcap;
            let silent = now - st.last_trade_ms > silence_ms;
            if collapsed || silent {
                unsub.push(mint.clone());
                st.subscribed = false;
                remove.push(mint.clone()); // free memory; data already persisted
            }
        } else if age >= survivor_ms && !st.survived {
            st.survived = true;
            new_survivors += 1;
        }
    }

    let dropped = remove.len();
    if !unsub.is_empty() {
        controller.unsubscribe_trades(&unsub, true);
    }
    for mint in remove {
        states.remove(&mint);
    }
    (dropped, new_survivors, controller.active_trade_subs())
}

fn install_shutdown(shutdown_tx: watch::Sender<bool>) {
    tokio::spawn(async move {
        #[cfg(unix)]
        {
            use tokio::signal::unix::{signal, SignalKind};
            let mut term = signal(SignalKind::terminate()).expect("SIGTERM handler");
            tokio::select! {
                _ = tokio::signal::ctrl_c() => {}
                _ = term.recv() => {}
            }
        }
        #[cfg(not(unix))]
        {
            let _ = tokio::signal::ctrl_c().await;
        }
        info!("shutdown signal received");
        let _ = shutdown_tx.send(true);
    });
}

/// Helius LaserStream WebSocket path: one `transactionSubscribe` on the pump.fun
/// program is a complete firehose — every token + every trade, no sub cap, no
/// per-token state machine. We persist new tokens always, and trades for tokens
/// we saw created (so age/outcome are computable). Storage is bounded by the
/// retention prune and, optionally, an early-trade window.
async fn run_helius(config: Config) -> Result<()> {
    if config.helius_api_key.is_none() && !config.helius_ws_url.contains("api-key=") {
        anyhow::bail!(
            "INGEST_SOURCE=helius needs HELIUS_API_KEY (or HELIUS_WS_URL containing api-key=)"
        );
    }
    let conn = db::open_writer(&config.db_path)?;
    let (write_tx, write_rx) = mpsc::unbounded_channel::<WriteMsg>();
    let writer = spawn_writer(conn, write_rx);

    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let (frame_tx, mut frame_rx) = mpsc::unbounded_channel::<Frame>();
    let supervisor = helius::start(
        config.helius_ws_url.clone(),
        config.pump_program_id.clone(),
        frame_tx,
        shutdown_rx.clone(),
    );
    let supervisor_handle = tokio::spawn(supervisor);

    install_shutdown(shutdown_tx.clone());
    info!(
        early_window_min = config.early_trade_window_minutes,
        retention_h = config.retention_hours,
        "helius ingest started"
    );

    // mint -> created_ms for every token seen this session (bounded by retention).
    let mut created: HashMap<String, i64> = HashMap::new();
    // recent trade signatures, to drop reconnect-overlap duplicates (bounded ring).
    let mut seen_sigs: HashSet<String> = HashSet::new();
    let mut sig_order: VecDeque<String> = VecDeque::new();
    const SIG_CAP: usize = 500_000;
    let early_ms = config.early_trade_window_ms();

    let mut buffer: Vec<model::Trade> = Vec::with_capacity(config.writer_flush_rows);
    let mut tokens_seen: u64 = 0;
    let mut trades_seen: u64 = 0;
    let mut trades_kept: u64 = 0;

    let mut flush = interval(Duration::from_millis(config.writer_flush_ms.max(50)));
    let mut sweep = interval(Duration::from_secs_f64((config.prune_interval_minutes * 60.0).max(5.0)));
    let mut snapshot = interval(Duration::from_secs_f64((config.snapshot_interval_minutes * 60.0).max(5.0)));
    let mut shutdown_rx2 = shutdown_rx.clone();

    loop {
        tokio::select! {
            Some(frame) = frame_rx.recv() => match frame {
                Frame::NewToken(t) => {
                    tokens_seen += 1;
                    created.insert(t.mint.clone(), t.created_ms);
                    let _ = write_tx.send(WriteMsg::NewToken(t));
                }
                Frame::Trade(t) => {
                    trades_seen += 1;
                    // Need a known creation time (age + re-training labels). Trades for
                    // tokens born before this session can't be aged, so drop them.
                    let cms = match created.get(&t.mint) {
                        Some(&c) => c,
                        None => continue,
                    };
                    if let Some(win) = early_ms {
                        if t.ts_ms - cms > win {
                            continue;
                        }
                    }
                    if let Some(sig) = t.signature.clone() {
                        if !seen_sigs.insert(sig.clone()) {
                            continue;
                        }
                        sig_order.push_back(sig);
                        if sig_order.len() > SIG_CAP {
                            if let Some(old) = sig_order.pop_front() {
                                seen_sigs.remove(&old);
                            }
                        }
                    }
                    trades_kept += 1;
                    buffer.push(t);
                    if buffer.len() >= config.writer_flush_rows {
                        let _ = write_tx.send(WriteMsg::Trades(std::mem::take(&mut buffer)));
                    }
                }
                Frame::Control { message, is_error } => {
                    if is_error { warn!(%message, "helius control"); } else { info!(%message, "helius control"); }
                }
                Frame::Unknown => {}
            },
            _ = flush.tick() => {
                if !buffer.is_empty() {
                    let _ = write_tx.send(WriteMsg::Trades(std::mem::take(&mut buffer)));
                }
            }
            _ = sweep.tick() => {
                let cutoff = now_ms() - config.retention_ms();
                created.retain(|_, v| *v >= cutoff);
                let _ = write_tx.send(WriteMsg::Prune { cutoff_ms: cutoff });
                info!(tracked_tokens = created.len(), tokens_seen, trades_seen, trades_kept, "sweep");
            }
            _ = snapshot.tick() => {
                let _ = write_tx.send(WriteMsg::Snapshot { path: config.snapshot_path.clone() });
            }
            _ = shutdown_rx2.changed() => {
                if *shutdown_rx2.borrow() { break; }
            }
        }
    }

    info!("shutting down helius ingest");
    if !buffer.is_empty() {
        let _ = write_tx.send(WriteMsg::Trades(std::mem::take(&mut buffer)));
    }
    let _ = write_tx.send(WriteMsg::Snapshot { path: config.snapshot_path.clone() });
    let _ = write_tx.send(WriteMsg::Shutdown);
    let _ = tokio::task::spawn_blocking(move || writer.join().ok()).await;
    supervisor_handle.abort();
    Ok(())
}
