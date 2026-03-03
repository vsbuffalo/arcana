pub mod fts;
pub mod schema;

use rusqlite::Connection;
use std::path::Path;

use crate::errors::Result;

pub struct Database {
    pub(crate) conn: Connection,
}

impl Database {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let conn = Connection::open(path)?;
        schema::init_schema(&conn)?;
        Ok(Database { conn })
    }

    pub fn open_in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        schema::init_schema(&conn)?;
        Ok(Database { conn })
    }

    pub fn set_query_only(&self, on: bool) -> Result<()> {
        let val = if on { "ON" } else { "OFF" };
        self.conn
            .execute_batch(&format!("PRAGMA query_only = {val}"))?;
        Ok(())
    }
}
