//! Configuration loaded from a local `.env` file and the process environment.

use std::env;
use std::fs;
use std::path::Path;

use anyhow::{Result, bail};

#[derive(Debug, Clone, PartialEq)]
pub struct Config {
    pub supabase_db_url: String,
    pub supabase_url: String,
    pub supabase_anon_key: String,
    pub clob_host: String,
    pub gamma_host: String,
    pub data_api_host: String,
    pub alchemy_polygon_url: String,
    pub workers: usize,
    pub batch_size: usize,
    pub verbose: bool,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            supabase_db_url: String::new(),
            supabase_url: String::new(),
            supabase_anon_key: String::new(),
            clob_host: "https://clob.polymarket.com".into(),
            gamma_host: "https://gamma-api.polymarket.com".into(),
            data_api_host: "https://data-api.polymarket.com".into(),
            alchemy_polygon_url: String::new(),
            workers: 4,
            batch_size: 500,
            verbose: false,
        }
    }
}

/// Load config. Values already set in the environment win over `.env`.
pub fn load() -> Result<Config> {
    load_dotenv(".env");

    let db_url = env::var("SUPABASE_DB_URL").unwrap_or_default();
    if db_url.is_empty() {
        bail!(
            "SUPABASE_DB_URL environment variable is required\n  \
             Copy .env.example to .env and fill in your Supabase connection string"
        );
    }

    let defaults = Config::default();
    Ok(Config {
        supabase_db_url: db_url,
        supabase_url: env::var("SUPABASE_URL").unwrap_or_default(),
        supabase_anon_key: env::var("SUPABASE_ANON_KEY").unwrap_or_default(),
        clob_host: env_or("CLOB_HOST", &defaults.clob_host),
        gamma_host: env_or("GAMMA_HOST", &defaults.gamma_host),
        data_api_host: env_or("DATA_API_HOST", &defaults.data_api_host),
        alchemy_polygon_url: env::var("ALCHEMY_POLYGON_URL").unwrap_or_default().trim().to_string(),
        ..defaults
    })
}

/// Set variables from a `KEY=value` file, skipping keys that already have a non-empty value.
pub fn load_dotenv(path: impl AsRef<Path>) {
    let Ok(text) = fs::read_to_string(path) else { return };
    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else { continue };
        let key = key.trim();
        if key.is_empty() || env::var(key).is_ok_and(|v| !v.is_empty()) {
            continue;
        }
        // SAFETY: called from the main thread at startup, before any other threads exist.
        unsafe { env::set_var(key, value.trim()) };
    }
}

fn env_or(key: &str, fallback: &str) -> String {
    match env::var(key) {
        Ok(v) if !v.is_empty() => v,
        _ => fallback.to_string(),
    }
}
