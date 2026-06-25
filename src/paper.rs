//! Phase 1+2 of the trading experiment: a **live** high-conviction screener wired
//! to a **paper-trading** engine. NO real money, NO keys, NO on-chain execution —
//! it opens *simulated* positions and logs realistic (fee+slippage-adjusted) P&L
//! so we can decide whether a live edge exists before funding anything.
//!
//! Decoupled from the data ingester: it opens its OWN Helius `transactionSubscribe`
//! stream (reusing [`crate::helius`]) so it can run/stop independently and never
//! risks the rolling-24h capture service.
//!
//! Flow per token: accumulate first-`entry_secs` features from the live Frame feed →
//! at the decision age, if it passes the entry rule, open a simulated buy at the
//! current market cap → manage the position to a take-profit / stop / timeout /
//! graduation exit → append the closed trade (after costs) to a JSONL log.

use std::collections::{HashMap, HashSet};
use std::fs::OpenOptions;
use std::io::Write;
use std::path::PathBuf;

use anyhow::Result;
use serde_json::json;
use tokio::sync::{mpsc, watch};
use tokio::time::{interval, Duration};
use tracing::{info, warn};

use crate::config::Config;
use crate::model::{Frame, Trade};
use crate::util::now_ms;

/// Tunable knobs (env defaults, optional CLI overrides). All time in ms internally.
#[derive(Clone, Debug)]
pub struct PaperParams {
    pub entry_age_ms: i64,
    pub min_buyers: usize,
    pub min_net_sol: f64,
    pub max_top_share: f64,
    pub take_profit: f64,
    pub stop_loss: f64,
    pub max_hold_ms: i64,
    pub grad_mcap: f64,
    pub fee_pct: f64,
    pub slip_pct: f64,
    pub position_sol: f64,
    pub max_concurrent: usize,
    pub out_path: PathBuf,
}

impl PaperParams {
    /// Resolve from env, then apply any CLI overrides.
    pub fn resolve(
        entry_secs: Option<u64>,
        min_buyers: Option<usize>,
        tp: Option<f64>,
        sl: Option<f64>,
        hold_secs: Option<u64>,
    ) -> Self {
        let entry_secs = entry_secs.unwrap_or_else(|| envu("PAPER_ENTRY_SECS", 60));
        let hold_secs = hold_secs.unwrap_or_else(|| envu("PAPER_HOLD_SECS", 300));
        PaperParams {
            entry_age_ms: (entry_secs as i64) * 1000,
            min_buyers: min_buyers.unwrap_or_else(|| envus("PAPER_MIN_BUYERS", 12)),
            min_net_sol: envf("PAPER_MIN_NET_SOL", 0.0),
            max_top_share: envf("PAPER_MAX_TOP_SHARE", 0.5),
            take_profit: tp.unwrap_or_else(|| envf("PAPER_TP", 0.5)),
            stop_loss: sl.unwrap_or_else(|| envf("PAPER_SL", 0.3)),
            max_hold_ms: (hold_secs as i64) * 1000,
            grad_mcap: envf("PAPER_GRAD_MCAP", 400.0),
            fee_pct: envf("PAPER_FEE_PCT", 0.01),
            slip_pct: envf("PAPER_SLIP_PCT", 0.015),
            position_sol: envf("PAPER_POSITION_SOL", 0.1),
            max_concurrent: envus("PAPER_MAX_CONCURRENT", 50),
            out_path: envs("PAPER_OUT", "./data/paper_trades.jsonl").into(),
        }
    }
}

struct Track {
    created_ms: i64,
    symbol: Option<String>,
    decided: bool,
    buyers: HashSet<String>,
    buy_sol: HashMap<String, f64>,
    net_sol: f64,
    last_mcap: f64,
    last_seen_ms: i64,
}

struct Pos {
    entry_ms: i64,
    entry_mcap: f64,
    symbol: String,
}

struct Engine {
    p: PaperParams,
    tracks: HashMap<String, Track>,
    positions: HashMap<String, Pos>,
    out: std::fs::File,
    n_signals: u64,
    n_closed: u64,
    n_wins: u64,
    total_pnl_sol: f64,
}

impl Engine {
    fn on_newtoken(&mut self, mint: String, symbol: Option<String>, created_ms: i64, mcap: f64) {
        self.tracks.entry(mint).or_insert(Track {
            created_ms,
            symbol,
            decided: false,
            buyers: HashSet::new(),
            buy_sol: HashMap::new(),
            net_sol: 0.0,
            last_mcap: mcap,
            last_seen_ms: now_ms(),
        });
    }

    fn on_trade(&mut self, t: Trade) {
        let now = now_ms();
        let mc = t.market_cap_sol;
        let mut need_decide = false;
        if let Some(tr) = self.tracks.get_mut(&t.mint) {
            if let Some(m) = mc {
                tr.last_mcap = m;
            }
            tr.last_seen_ms = now;
            if !tr.decided {
                let age = t.ts_ms - tr.created_ms;
                if age <= self.p.entry_age_ms {
                    tr.buyers.insert(t.trader.clone());
                    let s = t.sol_amount.unwrap_or(0.0);
                    if t.side == "buy" {
                        tr.net_sol += s;
                        *tr.buy_sol.entry(t.trader.clone()).or_default() += s;
                    } else {
                        tr.net_sol -= s;
                    }
                } else {
                    need_decide = true;
                }
            }
        }
        if need_decide {
            self.finalize_decision(&t.mint, now);
        }
        // Manage an open position the moment a fresh price prints.
        if let Some(cur) = mc {
            if let Some(reason) = self.exit_reason(&t.mint, cur, now) {
                self.close(&t.mint, cur, reason, now);
            }
        }
    }

    /// Close the entry window for one token; open a simulated position if it passes.
    fn finalize_decision(&mut self, mint: &str, now: i64) {
        let decision = match self.tracks.get_mut(mint) {
            Some(tr) if !tr.decided => {
                tr.decided = true;
                let buyers = tr.buyers.len();
                let net = tr.net_sol;
                let tot_buy: f64 = tr.buy_sol.values().sum();
                let max_buy = tr.buy_sol.values().cloned().fold(0.0_f64, f64::max);
                let top_share = if tot_buy > 0.0 { max_buy / tot_buy } else { 0.0 };
                let pass = buyers >= self.p.min_buyers
                    && net >= self.p.min_net_sol
                    && top_share < self.p.max_top_share;
                Some((pass, tr.last_mcap, tr.symbol.clone().unwrap_or_default(), buyers, net))
            }
            _ => None,
        };
        if let Some((pass, entry_mcap, symbol, buyers, net)) = decision {
            if pass
                && entry_mcap > 0.0
                && !self.positions.contains_key(mint)
                && self.positions.len() < self.p.max_concurrent
            {
                self.n_signals += 1;
                info!(
                    %symbol, mint, entry_mcap, buyers, net_sol = net,
                    open = self.positions.len() + 1, "PAPER BUY"
                );
                self.positions.insert(
                    mint.to_string(),
                    Pos { entry_ms: now, entry_mcap, symbol },
                );
            }
        }
    }

    fn exit_reason(&self, mint: &str, cur: f64, now: i64) -> Option<&'static str> {
        let pos = self.positions.get(mint)?;
        let e = pos.entry_mcap;
        if cur >= e * (1.0 + self.p.take_profit) {
            Some("take_profit")
        } else if cur <= e * (1.0 - self.p.stop_loss) {
            Some("stop_loss")
        } else if cur >= self.p.grad_mcap {
            Some("graduation")
        } else if now - pos.entry_ms >= self.p.max_hold_ms {
            Some("timeout")
        } else {
            None
        }
    }

    fn close(&mut self, mint: &str, cur: f64, reason: &str, now: i64) {
        if let Some(pos) = self.positions.remove(mint) {
            // Cost model: pay the spread on BOTH sides.
            let buy_eff = pos.entry_mcap * (1.0 + self.p.fee_pct + self.p.slip_pct);
            let sell_eff = cur * (1.0 - self.p.fee_pct - self.p.slip_pct);
            let pnl_frac = if buy_eff > 0.0 { sell_eff / buy_eff - 1.0 } else { 0.0 };
            let pnl_sol = self.p.position_sol * pnl_frac;
            self.n_closed += 1;
            if pnl_frac > 0.0 {
                self.n_wins += 1;
            }
            self.total_pnl_sol += pnl_sol;
            let hold_s = (now - pos.entry_ms) as f64 / 1000.0;
            let rec = json!({
                "ts_ms": now,
                "mint": mint,
                "symbol": pos.symbol,
                "entry_mcap": pos.entry_mcap,
                "exit_mcap": cur,
                "reason": reason,
                "hold_s": hold_s,
                "pnl_frac": pnl_frac,
                "pnl_sol": pnl_sol,
            });
            if writeln!(self.out, "{rec}").is_err() {
                warn!("paper: failed to write trade log");
            }
            let _ = self.out.flush();
        }
    }

    /// Periodic: decide windows that closed with no further trade, force timeouts,
    /// and drop stale trackers to bound memory.
    fn sweep(&mut self, now: i64) {
        let to_decide: Vec<String> = self
            .tracks
            .iter()
            .filter(|(_, tr)| !tr.decided && now - tr.created_ms > self.p.entry_age_ms)
            .map(|(m, _)| m.clone())
            .collect();
        for mint in to_decide {
            self.finalize_decision(&mint, now);
        }

        let exits: Vec<(String, f64)> = self
            .positions
            .iter()
            .filter_map(|(mint, pos)| {
                let cur = self.tracks.get(mint).map(|t| t.last_mcap).unwrap_or(pos.entry_mcap);
                self.exit_reason(mint, cur, now).map(|_| (mint.clone(), cur))
            })
            .collect();
        for (mint, cur) in exits {
            // re-derive reason at close time (price may have moved within the tick)
            let reason = self.exit_reason(&mint, cur, now).unwrap_or("timeout");
            self.close(&mint, cur, reason, now);
        }

        let ttl = self.p.entry_age_ms + self.p.max_hold_ms + 120_000;
        let open: HashSet<String> = self.positions.keys().cloned().collect();
        self.tracks
            .retain(|mint, tr| open.contains(mint) || now - tr.last_seen_ms <= ttl);
    }

    fn summary(&self) {
        let win_rate = if self.n_closed > 0 {
            100.0 * self.n_wins as f64 / self.n_closed as f64
        } else {
            0.0
        };
        info!(
            signals = self.n_signals,
            open = self.positions.len(),
            closed = self.n_closed,
            win_rate_pct = format!("{win_rate:.1}"),
            total_pnl_sol = format!("{:.4}", self.total_pnl_sol),
            "papertrade summary"
        );
    }
}

pub async fn run(config: Config, p: PaperParams) -> Result<()> {
    if config.helius_api_key.is_none() && !config.helius_ws_url.contains("api-key=") {
        anyhow::bail!("papertrade needs HELIUS_API_KEY (or HELIUS_WS_URL containing api-key=)");
    }
    if let Some(parent) = p.out_path.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    let out = OpenOptions::new().create(true).append(true).open(&p.out_path)?;

    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let (frame_tx, mut frame_rx) = mpsc::unbounded_channel::<Frame>();
    let supervisor = crate::helius::start(
        config.helius_ws_url.clone(),
        config.pump_program_id.clone(),
        frame_tx,
        shutdown_rx.clone(),
    );
    let supervisor_handle = tokio::spawn(supervisor);
    install_shutdown(shutdown_tx.clone());

    info!(
        entry_secs = p.entry_age_ms / 1000,
        min_buyers = p.min_buyers,
        max_top_share = p.max_top_share,
        tp = p.take_profit,
        sl = p.stop_loss,
        hold_secs = p.max_hold_ms / 1000,
        fee_pct = p.fee_pct,
        slip_pct = p.slip_pct,
        out = %p.out_path.display(),
        "papertrade started (SIMULATED — no real money)"
    );

    let mut engine = Engine {
        p,
        tracks: HashMap::new(),
        positions: HashMap::new(),
        out,
        n_signals: 0,
        n_closed: 0,
        n_wins: 0,
        total_pnl_sol: 0.0,
    };

    let mut tick = interval(Duration::from_secs(1));
    let mut summary = interval(Duration::from_secs(60));
    let mut shutdown_rx2 = shutdown_rx.clone();

    loop {
        tokio::select! {
            Some(frame) = frame_rx.recv() => match frame {
                Frame::NewToken(t) => {
                    engine.on_newtoken(t.mint, t.symbol, t.created_ms, t.market_cap_sol.unwrap_or(0.0));
                }
                Frame::Trade(t) => engine.on_trade(t),
                _ => {}
            },
            _ = tick.tick() => engine.sweep(now_ms()),
            _ = summary.tick() => engine.summary(),
            _ = shutdown_rx2.changed() => {
                if *shutdown_rx2.borrow() { break; }
            }
        }
    }

    engine.summary();
    info!("papertrade shutting down");
    supervisor_handle.abort();
    Ok(())
}

fn install_shutdown(tx: watch::Sender<bool>) {
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
        let _ = tx.send(true);
    });
}

fn envf(key: &str, def: f64) -> f64 {
    std::env::var(key).ok().and_then(|v| v.trim().parse().ok()).unwrap_or(def)
}
fn envu(key: &str, def: u64) -> u64 {
    std::env::var(key).ok().and_then(|v| v.trim().parse().ok()).unwrap_or(def)
}
fn envus(key: &str, def: usize) -> usize {
    std::env::var(key).ok().and_then(|v| v.trim().parse().ok()).unwrap_or(def)
}
fn envs(key: &str, def: &str) -> String {
    std::env::var(key).ok().filter(|v| !v.trim().is_empty()).unwrap_or_else(|| def.to_string())
}
