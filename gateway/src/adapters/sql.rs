//! A database as a source: a query run now and then, its rows arriving as JSON under the query's own
//! text. It only reads, because a site's historian is not this software's to change, and a statement
//! that tries anything else is refused here and by a read-only transaction there.
//!
//! `postgres://reader:{secret.db}@historian/plant` and `sqlite:///var/lib/plant/results.db`. Other
//! databases, by an external driver.

use crate::adapters::tags::{Tags, Trouble};
use async_trait::async_trait;
use ferrum_edge_contract::tags::read_only as reads_only;
use serde_json::{Map, Value, json};
use tokio::time::timeout;

/// Rows one query may bring back.
const MAX_ROWS: usize = 10_000;

pub struct Postgres {
    client: tokio_postgres::Client,
}

impl Postgres {
    pub async fn connect(address: &str) -> Result<Box<dyn Tags>, String> {
        let (client, connection) = timeout(super::tags::ANSWER, tokio_postgres::connect(address, tokio_postgres::NoTls))
            .await
            .map_err(|_| "it did not answer")?
            .map_err(|e| e.to_string())?;
        tokio::spawn(connection);
        client.batch_execute("set session characteristics as transaction read only").await.map_err(|e| e.to_string())?;
        Ok(Box::new(Postgres { client }))
    }
}

#[async_trait]
impl Tags for Postgres {
    async fn read(&mut self, sql: &str) -> Result<Value, Trouble> {
        if !reads_only(sql) {
            return Err(Trouble::Refused("this only reads: one select at a time".into()));
        }
        // Letting the database write its own JSON means every type it has comes through.
        let wrapped = format!(
            "select coalesce(jsonb_agg(row_to_json(t)), '[]')::text from (select * from ({}) q limit {MAX_ROWS}) t",
            sql.trim_end().trim_end_matches(';')
        );
        let row = timeout(super::tags::ANSWER * 6, self.client.query_one(&wrapped, &[])).await?;
        let row = row.map_err(|e| match e.as_db_error() {
            Some(said) => Trouble::Refused(said.message().to_string()),
            None => Trouble::Lost(e.to_string()),
        })?;
        let text: String = row.get(0);
        serde_json::from_str(&text).map_err(|e| Trouble::Refused(e.to_string()))
    }

    async fn write(&mut self, tag: &str, _: &Value) -> Result<(), Trouble> {
        Err(Trouble::Refused(format!("{tag}: a database is only read")))
    }
}

pub struct Sqlite {
    path: String,
}

impl Sqlite {
    pub async fn connect(path: &str) -> Result<Box<dyn Tags>, String> {
        let path = format!("/{}", path.split('?').next().unwrap_or(path).trim_start_matches('/'));
        rusqlite::Connection::open_with_flags(&path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).map_err(|e| format!("{path}: {e}"))?;
        Ok(Box::new(Sqlite { path }))
    }
}

fn run(path: &str, sql: &str) -> Result<Value, String> {
    let c = rusqlite::Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).map_err(|e| e.to_string())?;
    let mut statement = c.prepare(sql).map_err(|e| e.to_string())?;
    let names: Vec<String> = statement.column_names().iter().map(|n| n.to_string()).collect();
    let mut rows = statement.query([]).map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    while let Some(row) = rows.next().map_err(|e| e.to_string())? {
        let mut one = Map::new();
        for (i, name) in names.iter().enumerate() {
            let value = match row.get_ref(i).map_err(|e| e.to_string())? {
                rusqlite::types::ValueRef::Null => Value::Null,
                rusqlite::types::ValueRef::Integer(n) => json!(n),
                rusqlite::types::ValueRef::Real(n) => json!(n),
                rusqlite::types::ValueRef::Text(t) => json!(String::from_utf8_lossy(t)),
                rusqlite::types::ValueRef::Blob(b) => json!(hex::encode(b)),
            };
            one.insert(name.clone(), value);
        }
        out.push(Value::Object(one));
        if out.len() >= MAX_ROWS {
            break;
        }
    }
    Ok(Value::Array(out))
}

#[async_trait]
impl Tags for Sqlite {
    async fn read(&mut self, sql: &str) -> Result<Value, Trouble> {
        if !reads_only(sql) {
            return Err(Trouble::Refused("this only reads: one select at a time".into()));
        }
        let (path, sql) = (self.path.clone(), sql.to_string());
        tokio::task::spawn_blocking(move || run(&path, &sql)).await.map_err(|e| Trouble::Lost(e.to_string()))?.map_err(Trouble::Refused)
    }

    async fn write(&mut self, tag: &str, _: &Value) -> Result<(), Trouble> {
        Err(Trouble::Refused(format!("{tag}: a database is only read")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_one_read_at_a_time() {
        assert!(reads_only("select 1"));
        assert!(reads_only("  SELECT * from t;  "));
        assert!(reads_only("with a as (select 1) select * from a"));
        assert!(!reads_only("drop table t"));
        assert!(!reads_only("select 1; drop table t"));
        assert!(!reads_only("update t set a = 1"));
        assert!(!reads_only(""));
    }

    #[tokio::test]
    async fn sqlite_reads_and_cannot_write() {
        let path = std::env::temp_dir().join(format!("source-{}.db", uuid::Uuid::now_v7().simple()));
        let c = rusqlite::Connection::open(&path).unwrap();
        c.execute_batch("create table t (name text, n real); insert into t values ('a', 1.5), ('b', null);").unwrap();
        drop(c);
        let mut s = Sqlite::connect(path.to_str().unwrap()).await.unwrap();
        assert_eq!(s.read("select name, n from t order by name").await.ok(), Some(json!([{"name": "a", "n": 1.5}, {"name": "b", "n": null}])));
        assert!(matches!(s.read("delete from t").await, Err(Trouble::Refused(_))));
        assert!(matches!(s.read("select * from nothing").await, Err(Trouble::Refused(_))));
        assert!(matches!(s.write("x", &json!(1)).await, Err(Trouble::Refused(_))));
    }
}
