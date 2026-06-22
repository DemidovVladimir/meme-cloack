//! Dedicated DB-writer thread. DuckDB's `Connection` is `Send` but `!Sync`, so a
//! single thread owns it and all async producers funnel writes through a channel.
//! The Appender is created per-batch inside `db::append_trades` (never held across
//! a prune), which avoids the self-referential borrow and the
//! appender-visible-rows hazard.

use std::path::PathBuf;
use std::thread::JoinHandle;

use duckdb::Connection;
use tokio::sync::mpsc::UnboundedReceiver;
use tracing::{error, info};

use crate::db;
use crate::model::{NewToken, Trade};

pub enum WriteMsg {
    NewToken(NewToken),
    Trades(Vec<Trade>),
    Prune { cutoff_ms: i64 },
    Snapshot { path: PathBuf },
    Shutdown,
}

/// Spawn the writer thread. It owns `conn` and drains `rx` until `Shutdown`.
pub fn spawn_writer(conn: Connection, mut rx: UnboundedReceiver<WriteMsg>) -> JoinHandle<()> {
    std::thread::Builder::new()
        .name("db-writer".into())
        .spawn(move || {
            while let Some(msg) = rx.blocking_recv() {
                match msg {
                    WriteMsg::NewToken(t) => {
                        if let Err(e) = db::insert_new_token(&conn, &t) {
                            error!(mint = %t.mint, "insert new_token failed: {e:#}");
                        }
                    }
                    WriteMsg::Trades(batch) => {
                        let n = batch.len();
                        if let Err(e) = db::append_trades(&conn, &batch) {
                            error!("append {n} trades failed: {e:#}");
                        }
                    }
                    WriteMsg::Prune { cutoff_ms } => match db::prune(&conn, cutoff_ms) {
                        Ok(removed) => info!(removed, "prune complete"),
                        Err(e) => error!("prune failed: {e:#}"),
                    },
                    WriteMsg::Snapshot { path } => match db::write_snapshot(&conn, &path) {
                        Ok(()) => info!(path = %path.display(), "snapshot written"),
                        Err(e) => error!("snapshot failed: {e:#}"),
                    },
                    WriteMsg::Shutdown => {
                        info!("writer shutting down");
                        break;
                    }
                }
            }
        })
        .expect("spawn writer thread")
}
