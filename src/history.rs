use anyhow::Result;
use rusqlite::{params, Connection};
use serde_json::Value;
use std::path::Path;

pub struct History(Connection);
impl History {
    pub fn open(state: &Path) -> Result<Self> {
        crate::safety::absolute(state)?;
        std::fs::create_dir_all(state)?;
        crate::safety::no_symlinks(&state.join("history.sqlite"))?;
        let c = Connection::open(state.join("history.sqlite"))?;
        c.busy_timeout(std::time::Duration::from_secs(30))?;
        c.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL;
          CREATE TABLE IF NOT EXISTS events (
            id INTEGER PRIMARY KEY, timestamp TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
            app TEXT NOT NULL, kind TEXT NOT NULL, status TEXT NOT NULL, details TEXT NOT NULL);
          CREATE TABLE IF NOT EXISTS queue (
            id INTEGER PRIMARY KEY, app TEXT NOT NULL, commit_hash TEXT NOT NULL,
            status TEXT NOT NULL DEFAULT 'queued', error TEXT,
            created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')));")?;
        Ok(Self(c))
    }
    pub fn event(&self, app: &str, kind: &str, status: &str, details: Value) -> Result<()> {
        self.0.execute(
            "INSERT INTO events(app,kind,status,details) VALUES (?1,?2,?3,?4)",
            params![app, kind, status, details.to_string()],
        )?;
        eprintln!(
            "{}",
            serde_json::json!({"timestamp":crate::safety::stamp(),"app":app,"event":kind,"status":status,"details":details})
        );
        Ok(())
    }
    pub fn list(&self, app: Option<&str>) -> Result<Value> {
        let mut stmt = self.0.prepare("SELECT id,timestamp,app,kind,status,details FROM events WHERE (?1 IS NULL OR app=?1) ORDER BY id DESC LIMIT 200")?;
        let rows = stmt.query_map([app], |r| Ok(serde_json::json!({"id":r.get::<_,i64>(0)?,"timestamp":r.get::<_,String>(1)?,"app":r.get::<_,String>(2)?,"kind":r.get::<_,String>(3)?,"status":r.get::<_,String>(4)?,"details":r.get::<_,String>(5)?})))?;
        Ok(Value::Array(rows.collect::<rusqlite::Result<Vec<_>>>()?))
    }
    pub fn enqueue(&self, app: &str, commit: &str) -> Result<i64> {
        self.0.execute(
            "INSERT INTO queue(app,commit_hash) VALUES (?1,?2)",
            params![app, commit],
        )?;
        Ok(self.0.last_insert_rowid())
    }
    pub fn claim(&self) -> Result<Option<(i64, String, String)>> {
        // Caller holds the machine-wide release lock. Crashed running jobs are
        // marked interrupted, never silently rerun potentially non-idempotent code.
        self.0.execute("UPDATE queue SET status='interrupted',error='worker exited; enqueue explicitly to retry' WHERE status='running'", [])?;
        use rusqlite::OptionalExtension;
        let row = self
            .0
            .query_row(
                "SELECT id,app,commit_hash FROM queue WHERE status='queued' ORDER BY id LIMIT 1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()?;
        if let Some((id, _, _)) = &row {
            self.0
                .execute("UPDATE queue SET status='running' WHERE id=?1", [id])?;
        }
        Ok(row)
    }
    pub fn finish(&self, id: i64, error: Option<String>) -> Result<()> {
        self.0.execute(
            "UPDATE queue SET status=?1,error=?2 WHERE id=?3",
            params![
                if error.is_some() {
                    "failed"
                } else {
                    "complete"
                },
                error,
                id
            ],
        )?;
        Ok(())
    }
    pub fn queue(&self) -> Result<Value> {
        let mut stmt = self.0.prepare("SELECT id,app,commit_hash,status,error,created_at FROM queue ORDER BY id DESC LIMIT 200")?;
        let rows = stmt.query_map([], |r| Ok(serde_json::json!({"id":r.get::<_,i64>(0)?,"app":r.get::<_,String>(1)?,"commit":r.get::<_,String>(2)?,"status":r.get::<_,String>(3)?,"error":r.get::<_,Option<String>>(4)?,"timestamp":r.get::<_,String>(5)?})))?;
        Ok(Value::Array(rows.collect::<rusqlite::Result<Vec<_>>>()?))
    }
}
