use std::path::Path;
use std::sync::Mutex;

use rusqlite::{Connection, OptionalExtension, params};

use crate::Result;
use crate::event::{ChatId, EntityRef, Event, EventId, EventMeta, EventType, NewEvent, Status};
use crate::schema;

pub struct Store {
    conn: Mutex<Connection>,
}

impl Store {
    pub fn open(path: &Path) -> Result<Self> {
        let conn = Connection::open(path)?;
        schema::init(&conn)?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    pub fn open_in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        schema::init(&conn)?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    pub fn insert_event(&self, ev: &NewEvent) -> Result<(EventId, Option<ChatId>)> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;

        let chat_id = match ev.chat_external_id.as_deref() {
            Some(ext) => Some(upsert_chat(&tx, ext, ev.timestamp)?),
            None => None,
        };

        tx.execute(
            "INSERT INTO events (timestamp, event_type, source, content, status, chat_id)
             VALUES (?1, ?2, ?3, ?4, 'pending', ?5)",
            params![
                ev.timestamp,
                ev.event_type.as_str(),
                ev.source,
                ev.content,
                chat_id,
            ],
        )?;
        let id = tx.last_insert_rowid();

        if let Some(cid) = chat_id {
            tx.execute(
                "UPDATE chats SET last_event_at = ?1 WHERE id = ?2",
                params![ev.timestamp, cid],
            )?;
        }

        tx.commit()?;
        Ok((id, chat_id))
    }

    /// Fetch a pending event for summarization. Returns `None` if the row
    /// doesn't exist or is already `ready`.
    pub fn fetch_for_summarize(
        &self,
        id: EventId,
    ) -> Result<Option<(NewEvent, Option<ChatId>)>> {
        let conn = self.conn.lock().unwrap();
        let row = conn
            .query_row(
                "SELECT e.timestamp, e.event_type, e.source, e.content, c.external_id, e.chat_id, e.status
                 FROM events e LEFT JOIN chats c ON c.id = e.chat_id
                 WHERE e.id = ?1",
                params![id],
                |r| {
                    let ts: i64 = r.get(0)?;
                    let ty: String = r.get(1)?;
                    let src: Option<String> = r.get(2)?;
                    let content: String = r.get(3)?;
                    let chat_ext: Option<String> = r.get(4)?;
                    let chat_id: Option<i64> = r.get(5)?;
                    let status: String = r.get(6)?;
                    Ok((
                        NewEvent {
                            timestamp: ts,
                            event_type: EventType::from_str(&ty).unwrap_or(EventType::Note),
                            source: src,
                            content,
                            chat_external_id: chat_ext,
                        },
                        chat_id,
                        status,
                    ))
                },
            )
            .optional()?;
        match row {
            None => Ok(None),
            Some((_, _, status)) if status == "ready" => Ok(None),
            Some((ev, chat_id, _)) => Ok(Some((ev, chat_id))),
        }
    }

    /// Apply event meta + cascade counters in one transaction.
    /// Returns the entity ids touched (for caller to decide on rollups).
    pub fn apply_event_meta(
        &self,
        id: EventId,
        chat_id: Option<ChatId>,
        meta: &EventMeta,
    ) -> Result<Vec<i64>> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;
        tx.execute(
            "UPDATE events SET summary = ?1, status = 'ready' WHERE id = ?2",
            params![meta.summary, id],
        )?;
        if let Some(cid) = chat_id {
            tx.execute(
                "UPDATE chats SET events_since_summary = events_since_summary + 1
                 WHERE id = ?1",
                params![cid],
            )?;
        }
        for topic in &meta.topics {
            tx.execute(
                "INSERT OR IGNORE INTO topics (name) VALUES (?1)",
                params![topic],
            )?;
            let topic_id: i64 = tx.query_row(
                "SELECT id FROM topics WHERE name = ?1",
                params![topic],
                |r| r.get(0),
            )?;
            tx.execute(
                "INSERT OR IGNORE INTO event_topics (event_id, topic_id) VALUES (?1, ?2)",
                params![id, topic_id],
            )?;
        }
        let mut entity_ids = Vec::with_capacity(meta.entities.len());
        for ent in &meta.entities {
            tx.execute(
                "INSERT OR IGNORE INTO entities (name, kind) VALUES (?1, ?2)",
                params![ent.name, ent.kind],
            )?;
            let ent_id: i64 = tx.query_row(
                "SELECT id FROM entities WHERE name = ?1 AND kind IS ?2",
                params![ent.name, ent.kind],
                |r| r.get(0),
            )?;
            tx.execute(
                "INSERT OR IGNORE INTO event_entities (event_id, entity_id) VALUES (?1, ?2)",
                params![id, ent_id],
            )?;
            tx.execute(
                "UPDATE entities SET mentions_since_summary = mentions_since_summary + 1,
                    status = CASE WHEN status='ready' THEN 'stale' ELSE status END
                 WHERE id = ?1",
                params![ent_id],
            )?;
            entity_ids.push(ent_id);
        }
        tx.commit()?;
        Ok(entity_ids)
    }

    pub fn chat_due_for_summary(&self, chat_id: ChatId, threshold: i64) -> Result<bool> {
        let conn = self.conn.lock().unwrap();
        let n: i64 = conn.query_row(
            "SELECT events_since_summary FROM chats WHERE id = ?1",
            params![chat_id],
            |r| r.get(0),
        )?;
        Ok(n >= threshold)
    }

    /// Entity is due for summary if it has no summary yet (cold start) or
    /// has accumulated `threshold` new mentions since the last rollup.
    pub fn entity_due_for_summary(&self, entity_id: i64, threshold: i64) -> Result<bool> {
        let conn = self.conn.lock().unwrap();
        let (has_summary, n): (bool, i64) = conn.query_row(
            "SELECT summary IS NOT NULL, mentions_since_summary FROM entities WHERE id = ?1",
            params![entity_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        Ok(!has_summary || n >= threshold)
    }

    pub fn mark_event_failed(&self, id: EventId) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE events SET status = 'failed' WHERE id = ?1",
            params![id],
        )?;
        Ok(())
    }

    pub fn get_event(&self, id: EventId) -> Result<Option<Event>> {
        let conn = self.conn.lock().unwrap();
        let base = conn
            .query_row(
                "SELECT id, timestamp, event_type, source, content, summary, status, chat_id
                 FROM events WHERE id = ?1",
                params![id],
                |r| {
                    let id: i64 = r.get(0)?;
                    let ts: i64 = r.get(1)?;
                    let ty: String = r.get(2)?;
                    let src: Option<String> = r.get(3)?;
                    let content: String = r.get(4)?;
                    let summary: Option<String> = r.get(5)?;
                    let status: String = r.get(6)?;
                    let chat_id: Option<i64> = r.get(7)?;
                    Ok((id, ts, ty, src, content, summary, status, chat_id))
                },
            )
            .optional()?;
        let Some((id, timestamp, ty, source, content, summary, status, chat_id)) = base else {
            return Ok(None);
        };
        let topics: Vec<String> = conn
            .prepare(
                "SELECT t.name FROM event_topics et JOIN topics t ON t.id = et.topic_id
                 WHERE et.event_id = ?1",
            )?
            .query_map(params![id], |r| r.get::<_, String>(0))?
            .collect::<std::result::Result<_, _>>()?;
        let entities: Vec<EntityRef> = conn
            .prepare(
                "SELECT e.name, e.kind FROM event_entities ee JOIN entities e ON e.id = ee.entity_id
                 WHERE ee.event_id = ?1",
            )?
            .query_map(params![id], |r| {
                Ok(EntityRef {
                    name: r.get(0)?,
                    kind: r.get(1)?,
                })
            })?
            .collect::<std::result::Result<_, _>>()?;
        Ok(Some(Event {
            id,
            timestamp,
            event_type: EventType::from_str(&ty).unwrap_or(EventType::Note),
            source,
            content,
            summary,
            status: Status::from_str(&status).unwrap_or(Status::Pending),
            chat_id,
            topics,
            entities,
        }))
    }

    /// Run a read-only SQL query, return rows as JSON values.
    pub fn query_sql(&self, sql: &str) -> Result<Vec<serde_json::Map<String, serde_json::Value>>> {
        self.query_sql_with_params(sql, &[])
    }

    /// Run a read-only SQL query with positional parameters bound from JSON.
    pub fn query_sql_with_params(
        &self,
        sql: &str,
        params: &[serde_json::Value],
    ) -> Result<Vec<serde_json::Map<String, serde_json::Value>>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(sql)?;
        let col_names: Vec<String> = stmt.column_names().iter().map(|s| s.to_string()).collect();
        let bound: Vec<rusqlite::types::Value> = params.iter().map(json_to_sql_value).collect();
        let bound_refs: Vec<&dyn rusqlite::ToSql> =
            bound.iter().map(|v| v as &dyn rusqlite::ToSql).collect();
        let rows = stmt.query_map(bound_refs.as_slice(), |row| {
            let mut obj = serde_json::Map::new();
            for (i, name) in col_names.iter().enumerate() {
                let v = row.get_ref(i)?;
                obj.insert(name.clone(), value_ref_to_json(v));
            }
            Ok(obj)
        })?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    pub fn chat_context(
        &self,
        chat_id: ChatId,
    ) -> Result<(Option<String>, Option<String>, Vec<crate::summarizer::EventDigest>)> {
        let conn = self.conn.lock().unwrap();
        let (title, summary, since): (Option<String>, Option<String>, i64) = conn.query_row(
            "SELECT title, summary, events_since_summary FROM chats WHERE id = ?1",
            params![chat_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )?;
        let mut stmt = conn.prepare(
            "SELECT timestamp, event_type, coalesce(summary, substr(content,1,200))
             FROM events WHERE chat_id = ?1 AND status='ready'
             ORDER BY id DESC LIMIT ?2",
        )?;
        let mut digs: Vec<crate::summarizer::EventDigest> = stmt
            .query_map(params![chat_id, since], |r| {
                Ok(crate::summarizer::EventDigest {
                    timestamp: r.get(0)?,
                    event_type: r.get(1)?,
                    summary: r.get(2)?,
                })
            })?
            .collect::<std::result::Result<_, _>>()?;
        digs.reverse();
        Ok((title, summary, digs))
    }

    pub fn apply_chat_summary(
        &self,
        chat_id: ChatId,
        s: &crate::event::ChatSummary,
    ) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE chats SET title = ?1, summary = ?2,
                events_since_summary = 0, status = 'ready' WHERE id = ?3",
            params![s.title, s.summary, chat_id],
        )?;
        Ok(())
    }

    pub fn entity_context(
        &self,
        entity_id: i64,
    ) -> Result<(String, Option<String>, Option<String>, Vec<crate::summarizer::EventDigest>)> {
        let conn = self.conn.lock().unwrap();
        let (name, kind, summary, since): (String, Option<String>, Option<String>, i64) =
            conn.query_row(
                "SELECT name, kind, summary, mentions_since_summary FROM entities WHERE id = ?1",
                params![entity_id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )?;
        let mut stmt = conn.prepare(
            "SELECT e.timestamp, e.event_type, coalesce(e.summary, substr(e.content,1,200))
             FROM events e JOIN event_entities ee ON ee.event_id = e.id
             WHERE ee.entity_id = ?1 AND e.status='ready'
             ORDER BY e.id DESC LIMIT ?2",
        )?;
        let mut digs: Vec<crate::summarizer::EventDigest> = stmt
            .query_map(params![entity_id, since], |r| {
                Ok(crate::summarizer::EventDigest {
                    timestamp: r.get(0)?,
                    event_type: r.get(1)?,
                    summary: r.get(2)?,
                })
            })?
            .collect::<std::result::Result<_, _>>()?;
        digs.reverse();
        Ok((name, kind, summary, digs))
    }

    pub fn apply_entity_summary(
        &self,
        entity_id: i64,
        s: &crate::event::EntitySummary,
    ) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE entities SET summary = ?1, mentions_since_summary = 0, status='ready'
             WHERE id = ?2",
            params![s.summary, entity_id],
        )?;
        Ok(())
    }
}

fn upsert_chat(
    tx: &rusqlite::Transaction<'_>,
    external_id: &str,
    timestamp: i64,
) -> Result<ChatId> {
    if let Some(id) = tx
        .query_row(
            "SELECT id FROM chats WHERE external_id = ?1",
            params![external_id],
            |r| r.get::<_, i64>(0),
        )
        .optional()?
    {
        return Ok(id);
    }
    tx.execute(
        "INSERT INTO chats (external_id, started_at, last_event_at, status)
         VALUES (?1, ?2, ?2, 'pending')",
        params![external_id, timestamp],
    )?;
    Ok(tx.last_insert_rowid())
}

fn json_to_sql_value(v: &serde_json::Value) -> rusqlite::types::Value {
    use rusqlite::types::Value;
    match v {
        serde_json::Value::Null => Value::Null,
        serde_json::Value::Bool(b) => Value::Integer(if *b { 1 } else { 0 }),
        serde_json::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Value::Integer(i)
            } else if let Some(f) = n.as_f64() {
                Value::Real(f)
            } else {
                Value::Null
            }
        }
        serde_json::Value::String(s) => Value::Text(s.clone()),
        serde_json::Value::Array(_) | serde_json::Value::Object(_) => {
            Value::Text(v.to_string())
        }
    }
}

fn value_ref_to_json(v: rusqlite::types::ValueRef<'_>) -> serde_json::Value {
    use rusqlite::types::ValueRef;
    match v {
        ValueRef::Null => serde_json::Value::Null,
        ValueRef::Integer(i) => serde_json::Value::from(i),
        ValueRef::Real(f) => serde_json::Number::from_f64(f)
            .map(serde_json::Value::Number)
            .unwrap_or(serde_json::Value::Null),
        ValueRef::Text(t) => serde_json::Value::String(String::from_utf8_lossy(t).into_owned()),
        ValueRef::Blob(b) => serde_json::Value::String(format!("<blob {} bytes>", b.len())),
    }
}
