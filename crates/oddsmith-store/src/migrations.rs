//! Runs the `.sql` files in a directory, in name order.

use std::fs;
use std::path::Path;

use anyhow::{Context, Result, bail};

use crate::db::Db;

impl Db {
    pub fn run_migrations(&self, dir: impl AsRef<Path>) -> Result<()> {
        let dir = dir.as_ref();
        if !dir.is_dir() {
            bail!("reading migrations directory {}: not a directory", dir.display());
        }
        let mut files: Vec<_> = fs::read_dir(dir)
            .with_context(|| format!("reading migrations directory {}", dir.display()))?
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.is_file() && p.extension().is_some_and(|x| x == "sql"))
            .collect();
        files.sort();
        for path in files {
            let name = path.file_name().unwrap_or_default().to_string_lossy().into_owned();
            let sql = fs::read_to_string(&path).with_context(|| format!("reading migration {name}"))?;
            self.batch_execute(&sql).with_context(|| format!("executing migration {name}"))?;
            println!("  applied: {name}");
        }
        Ok(())
    }
}
