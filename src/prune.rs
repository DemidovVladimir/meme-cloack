//! `prune` subcommand: manual retention sweep + snapshot rebuild.
//!
//! NOTE: this opens the hot DB read-write, so it can only run when the ingester
//! is stopped (DuckDB's exclusive file lock). In normal operation the running
//! ingester prunes and rewrites the snapshot itself on its timer; this is for
//! manual maintenance.

use anyhow::Result;
use tracing::info;

use crate::config::Config;
use crate::db;
use crate::util::now_ms;

pub fn run(config: &Config) -> Result<()> {
    let conn = db::open_writer(&config.db_path)?;
    let removed = db::prune(&conn, now_ms() - config.retention_ms())?;
    db::write_snapshot(&conn, &config.snapshot_path)?;
    info!(removed, snapshot = %config.snapshot_path.display(), "manual prune + snapshot complete");
    println!("pruned {removed} rows; snapshot at {}", config.snapshot_path.display());
    Ok(())
}
