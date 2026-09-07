//! Durable local CI records. Worker execution is deliberately outside this module.
use anyhow::{bail, Context, Result};
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    io::Read,
    path::{Path, PathBuf},
};

pub struct Store {
    db: Connection,
    state: PathBuf,
}

impl Store {
    pub fn open(state: &Path) -> Result<Self> {
        crate::safety::absolute(state)?;
        std::fs::create_dir_all(state)?;
        for suffix in ["ci.sqlite", "ci.sqlite-wal", "ci.sqlite-shm"] {
            crate::safety::no_symlinks(&state.join(suffix))?;
        }
        let database = state.join("ci.sqlite");
        if !database.exists() {
            crate::safety::create_new(&database, b"")?;
        }
        let db = Connection::open(database)?;
        db.busy_timeout(std::time::Duration::from_secs(30))?;
        let version: i64 = db.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        if version > 1 {
            bail!("CI database schema {version} is newer than this binary supports");
        }
        db.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL; PRAGMA foreign_keys=ON;
            CREATE TABLE IF NOT EXISTS runs (
              id INTEGER PRIMARY KEY, target TEXT NOT NULL, commit_hash TEXT NOT NULL,
              plan_digest TEXT NOT NULL, dedupe_key TEXT NOT NULL, context TEXT NOT NULL,
              status TEXT NOT NULL DEFAULT 'queued', cancellation_requested INTEGER NOT NULL DEFAULT 0,
              error TEXT, created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
              started_at TEXT, finished_at TEXT, UNIQUE(target,commit_hash,plan_digest,dedupe_key));
            CREATE TABLE IF NOT EXISTS stages (
              run_id INTEGER NOT NULL REFERENCES runs(id), name TEXT NOT NULL, status TEXT NOT NULL,
              details TEXT NOT NULL, updated_at TEXT NOT NULL, PRIMARY KEY(run_id,name));
            CREATE TABLE IF NOT EXISTS events (
              id INTEGER PRIMARY KEY AUTOINCREMENT, run_id INTEGER NOT NULL REFERENCES runs(id),
              stage TEXT, kind TEXT NOT NULL, payload TEXT NOT NULL,
              timestamp TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')));
            CREATE INDEX IF NOT EXISTS events_run ON events(run_id,id);
            CREATE TABLE IF NOT EXISTS artifacts (
              id INTEGER PRIMARY KEY, run_id INTEGER NOT NULL REFERENCES runs(id), stage TEXT NOT NULL,
              kind TEXT NOT NULL, path TEXT NOT NULL, sha256 TEXT NOT NULL, size INTEGER NOT NULL,
              created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')));
            PRAGMA user_version=1;")?;
        Ok(Self {
            db,
            state: state.to_path_buf(),
        })
    }

    pub fn enqueue(
        &self,
        target: &str,
        commit: &str,
        plan_digest: &str,
        key: &str,
        context: Value,
    ) -> Result<i64> {
        let tx = self.db.unchecked_transaction()?;
        let inserted=tx.execute("INSERT INTO runs(target,commit_hash,plan_digest,dedupe_key,context) VALUES (?1,?2,?3,?4,?5) ON CONFLICT(target,commit_hash,plan_digest,dedupe_key) DO NOTHING", params![target,commit,plan_digest,key,context.to_string()])?;
        let id=tx.query_row("SELECT id FROM runs WHERE target=?1 AND commit_hash=?2 AND plan_digest=?3 AND dedupe_key=?4",params![target,commit,plan_digest,key],|r| r.get::<_,i64>(0))?;
        if inserted == 1 {
            tx.execute(
                "INSERT INTO events(run_id,kind,payload) VALUES (?1,'queued','{}')",
                [id],
            )?;
        }
        tx.commit()?;
        Ok(id)
    }

    pub fn claim(&self) -> Result<Option<Value>> {
        // One UPDATE statement is atomic across independent connections/processes.
        let tx = self.db.unchecked_transaction()?;
        let id = tx.query_row("UPDATE runs SET status='running',started_at=strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE id=(SELECT id FROM runs WHERE status='queued' ORDER BY id LIMIT 1) AND status='queued' RETURNING id",[],|r| r.get::<_,i64>(0)).optional()?;
        if let Some(id) = id {
            tx.execute(
                "INSERT INTO events(run_id,kind,payload) VALUES (?1,'running','{}')",
                [id],
            )?;
        }
        tx.commit()?;
        id.map(|id| self.run(id)).transpose()
    }

    pub fn list(&self) -> Result<Value> {
        let mut stmt = self
            .db
            .prepare("SELECT id FROM runs ORDER BY id DESC LIMIT 200")?;
        let ids = stmt
            .query_map([], |r| r.get::<_, i64>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(Value::Array(
            ids.into_iter()
                .map(|id| self.run(id))
                .collect::<Result<_>>()?,
        ))
    }

    pub fn run(&self, id: i64) -> Result<Value> {
        let mut value = self.db.query_row("SELECT id,target,commit_hash,plan_digest,status,context,cancellation_requested,error,created_at,started_at,finished_at FROM runs WHERE id=?1",[id],|r| Ok(json!({
            "id":r.get::<_,i64>(0)?,"target":r.get::<_,String>(1)?,"commit":r.get::<_,String>(2)?,
            "plan_digest":r.get::<_,String>(3)?,"status":r.get::<_,String>(4)?,"context":parse(r.get(5)?),
            "cancellation_requested":r.get::<_,bool>(6)?,"error":r.get::<_,Option<String>>(7)?,
            "created_at":r.get::<_,String>(8)?,"started_at":r.get::<_,Option<String>>(9)?,"finished_at":r.get::<_,Option<String>>(10)?
        }))).with_context(|| format!("CI run {id} not found"))?;
        let mut stmt = self.db.prepare(
            "SELECT name,status,details,updated_at FROM stages WHERE run_id=?1 ORDER BY rowid",
        )?;
        value["stages"] = Value::Array(stmt.query_map([id],|r| Ok(json!({"name":r.get::<_,String>(0)?,"status":r.get::<_,String>(1)?,"details":parse(r.get(2)?),"updated_at":r.get::<_,String>(3)?})))?.collect::<rusqlite::Result<_>>()?);
        let mut stmt = self
            .db
            .prepare("SELECT id FROM artifacts WHERE run_id=?1 ORDER BY id")?;
        let ids = stmt
            .query_map([id], |r| r.get::<_, i64>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        value["artifacts"] = Value::Array(
            ids.into_iter()
                .map(|i| self.artifact_info(i))
                .collect::<Result<_>>()?,
        );
        Ok(value)
    }

    pub fn event(&self, run: i64, stage: Option<&str>, kind: &str, payload: Value) -> Result<()> {
        self.db.execute(
            "INSERT INTO events(run_id,stage,kind,payload) VALUES (?1,?2,?3,?4)",
            params![run, stage, kind, payload.to_string()],
        )?;
        Ok(())
    }

    pub fn events(&self, run: i64, after: i64) -> Result<Value> {
        let mut stmt = self.db.prepare("SELECT id,stage,kind,payload,timestamp FROM events WHERE run_id=?1 AND id>?2 ORDER BY id LIMIT 1000")?;
        let rows=stmt.query_map(params![run,after],|r| Ok(json!({"id":r.get::<_,i64>(0)?,"run_id":run,"stage":r.get::<_,Option<String>>(1)?,"kind":r.get::<_,String>(2)?,"payload":parse(r.get(3)?),"timestamp":r.get::<_,String>(4)?})))?.collect::<rusqlite::Result<_>>()?;
        Ok(Value::Array(rows))
    }

    pub fn stage(&self, run: i64, stage: &str, status: &str, details: Value) -> Result<()> {
        let tx = self.db.unchecked_transaction()?;
        tx.execute("INSERT INTO stages(run_id,name,status,details,updated_at) VALUES (?1,?2,?3,?4,strftime('%Y-%m-%dT%H:%M:%fZ','now')) ON CONFLICT(run_id,name) DO UPDATE SET status=excluded.status,details=excluded.details,updated_at=excluded.updated_at",params![run,stage,status,details.to_string()])?;
        tx.execute(
            "INSERT INTO events(run_id,stage,kind,payload) VALUES (?1,?2,'stage',?3)",
            params![
                run,
                stage,
                json!({"status":status,"details":details}).to_string()
            ],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn finish(&self, run: i64, status: &str, error: Option<&str>) -> Result<()> {
        if !matches!(
            status,
            "succeeded"
                | "passed"
                | "blocked"
                | "failed"
                | "cancelled"
                | "interrupted"
                | "complete"
        ) {
            bail!("invalid terminal run status: {status}");
        }
        let tx = self.db.unchecked_transaction()?;
        let changed = tx.execute("UPDATE runs SET status=?2,error=?3,finished_at=strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE id=?1 AND status='running'",params![run,status,error])?;
        if changed != 1 {
            bail!("CI run {run} is not running");
        }
        tx.execute("UPDATE stages SET status='blocked',updated_at=strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE run_id=?1 AND status IN ('pending','queued')",[run])?;
        tx.execute(
            "INSERT INTO events(run_id,kind,payload) VALUES (?1,?2,?3)",
            params![run, status, json!({"error":error}).to_string()],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn artifact(&self, run: i64, stage: &str, kind: &str, path: &Path) -> Result<i64> {
        crate::safety::absolute(path)?;
        if !path.starts_with(&self.state) || path == self.state {
            bail!("artifact must be inside CI state directory");
        }
        let mut file = std::fs::File::open(path)?;
        let metadata = file.metadata()?;
        if !metadata.is_file() {
            bail!("artifact is not a regular file: {}", path.display());
        }
        let mut digest = Sha256::new();
        let mut buf = [0_u8; 65536];
        let mut size = 0_i64;
        loop {
            let n = file.read(&mut buf)?;
            if n == 0 {
                break;
            }
            digest.update(&buf[..n]);
            size += n as i64;
        }
        let hash = format!("{:x}", digest.finalize());
        let tx = self.db.unchecked_transaction()?;
        tx.execute(
            "INSERT INTO artifacts(run_id,stage,kind,path,sha256,size) VALUES (?1,?2,?3,?4,?5,?6)",
            params![
                run,
                stage,
                kind,
                path.to_str().context("artifact path is not UTF-8")?,
                hash,
                size
            ],
        )?;
        let id = tx.last_insert_rowid();
        tx.execute(
            "INSERT INTO events(run_id,stage,kind,payload) VALUES (?1,?2,'artifact',?3)",
            params![
                run,
                stage,
                json!({"artifact_id":id,"kind":kind,"sha256":hash,"size":size}).to_string()
            ],
        )?;
        tx.commit()?;
        Ok(id)
    }

    pub fn artifact_info(&self, id: i64) -> Result<Value> {
        self.db.query_row("SELECT id,run_id,stage,kind,path,sha256,size,created_at FROM artifacts WHERE id=?1",[id],|r|Ok(json!({"id":r.get::<_,i64>(0)?,"run_id":r.get::<_,i64>(1)?,"stage":r.get::<_,String>(2)?,"kind":r.get::<_,String>(3)?,"path":r.get::<_,String>(4)?,"sha256":r.get::<_,String>(5)?,"size":r.get::<_,i64>(6)?,"created_at":r.get::<_,String>(7)?}))).with_context(||format!("CI artifact {id} not found"))
    }

    pub fn cancel(&self, id: i64) -> Result<()> {
        let tx = self.db.unchecked_transaction()?;
        let changed = tx.execute("UPDATE runs SET cancellation_requested=1,status=CASE WHEN status='queued' THEN 'cancelled' ELSE status END,finished_at=CASE WHEN status='queued' THEN strftime('%Y-%m-%dT%H:%M:%fZ','now') ELSE finished_at END WHERE id=?1 AND status IN ('queued','running') AND cancellation_requested=0",[id])?;
        if changed == 0 {
            self.run(id)?;
        } else {
            tx.execute(
                "INSERT INTO events(run_id,kind,payload) VALUES (?1,'cancellation_requested','{}')",
                [id],
            )?;
            tx.execute("UPDATE stages SET status='blocked',updated_at=strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE run_id=?1 AND status IN ('pending','queued') AND EXISTS (SELECT 1 FROM runs WHERE id=?1 AND status='cancelled')",[id])?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn cancelled(&self, id: i64) -> Result<bool> {
        Ok(self.db.query_row(
            "SELECT cancellation_requested FROM runs WHERE id=?1",
            [id],
            |r| r.get(0),
        )?)
    }

    /// Caller must hold the exclusive worker lock before declaring old work interrupted.
    pub fn recover_interrupted(&self) -> Result<()> {
        let tx = self.db.unchecked_transaction()?;
        tx.execute("INSERT INTO events(run_id,kind,payload) SELECT id,'interrupted','{}' FROM runs WHERE status='running'",[])?;
        tx.execute("UPDATE stages SET status='interrupted',updated_at=strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE status='running' AND run_id IN (SELECT id FROM runs WHERE status='running')",[])?;
        tx.execute("UPDATE stages SET status='blocked',updated_at=strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE status IN ('pending','queued') AND run_id IN (SELECT id FROM runs WHERE status='running')",[])?;
        tx.execute("UPDATE runs SET status='interrupted',error='worker stopped; enqueue explicitly to retry',finished_at=strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE status='running'",[])?;
        tx.commit()?;
        Ok(())
    }
}

fn parse(value: String) -> Value {
    serde_json::from_str(&value).unwrap_or(Value::String(value))
}
