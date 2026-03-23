//! The global CLI flags every command receives.

use anyhow::Result;

use crate::config::{self, Config};
use crate::db::Db;

/// Global flags (`-v/--verbose`, `--db-url`) shared by every command.
#[derive(Debug, Clone, Default)]
pub struct Ctx {
    pub verbose: bool,
    pub db_url: Option<String>,
}

impl Ctx {
    /// Load the config and apply the global flag overrides.
    pub fn config(&self) -> Result<Config> {
        let mut cfg = config::load()?;
        if let Some(url) = self.db_url.as_deref().filter(|u| !u.is_empty()) {
            cfg.supabase_db_url = url.to_string();
        }
        cfg.verbose = self.verbose;
        Ok(cfg)
    }

    /// Open the connection pool, announcing it in verbose mode.
    pub fn connect(&self, cfg: &Config) -> Result<Db> {
        if cfg.verbose {
            println!("connecting to database...");
        }
        let db = Db::connect(&cfg.supabase_db_url)?;
        if cfg.verbose {
            println!("connected");
        }
        Ok(db)
    }
}
