use rusqlite::Connection;

use crate::Result;

pub fn init(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        r#"
        PRAGMA journal_mode = WAL;
        PRAGMA foreign_keys = ON;

        CREATE TABLE IF NOT EXISTS chats (
            id              INTEGER PRIMARY KEY,
            external_id     TEXT UNIQUE,
            title           TEXT,
            summary         TEXT,
            started_at      INTEGER NOT NULL,
            last_event_at   INTEGER NOT NULL,
            events_since_summary INTEGER NOT NULL DEFAULT 0,
            status          TEXT NOT NULL DEFAULT 'pending'
        );

        CREATE TABLE IF NOT EXISTS events (
            id          INTEGER PRIMARY KEY,
            timestamp   INTEGER NOT NULL,
            event_type  TEXT NOT NULL,
            source      TEXT,
            content     TEXT NOT NULL,
            summary     TEXT,
            status      TEXT NOT NULL DEFAULT 'pending',
            chat_id     INTEGER REFERENCES chats(id) ON DELETE SET NULL
        );

        CREATE INDEX IF NOT EXISTS idx_events_timestamp ON events(timestamp);
        CREATE INDEX IF NOT EXISTS idx_events_type ON events(event_type);
        CREATE INDEX IF NOT EXISTS idx_events_status ON events(status);
        CREATE INDEX IF NOT EXISTS idx_events_chat ON events(chat_id);

        CREATE TABLE IF NOT EXISTS topics (
            id   INTEGER PRIMARY KEY,
            name TEXT UNIQUE NOT NULL
        );

        CREATE TABLE IF NOT EXISTS event_topics (
            event_id INTEGER NOT NULL REFERENCES events(id) ON DELETE CASCADE,
            topic_id INTEGER NOT NULL REFERENCES topics(id) ON DELETE CASCADE,
            PRIMARY KEY (event_id, topic_id)
        );
        CREATE INDEX IF NOT EXISTS idx_event_topics_topic ON event_topics(topic_id);

        CREATE TABLE IF NOT EXISTS entities (
            id      INTEGER PRIMARY KEY,
            name    TEXT NOT NULL,
            kind    TEXT,
            summary TEXT,
            mentions_since_summary INTEGER NOT NULL DEFAULT 0,
            status  TEXT NOT NULL DEFAULT 'pending',
            UNIQUE(name, kind)
        );

        CREATE TABLE IF NOT EXISTS event_entities (
            event_id  INTEGER NOT NULL REFERENCES events(id) ON DELETE CASCADE,
            entity_id INTEGER NOT NULL REFERENCES entities(id) ON DELETE CASCADE,
            PRIMARY KEY (event_id, entity_id)
        );
        CREATE INDEX IF NOT EXISTS idx_event_entities_entity ON event_entities(entity_id);

        CREATE VIEW IF NOT EXISTS chat_messages AS
            SELECT id, timestamp, event_type, source, content, summary, status, chat_id
            FROM events
            WHERE event_type IN ('user_message', 'agent_message');

        CREATE VIRTUAL TABLE IF NOT EXISTS events_fts USING fts5(
            content, summary,
            content='events', content_rowid='id'
        );

        CREATE TRIGGER IF NOT EXISTS events_ai AFTER INSERT ON events BEGIN
            INSERT INTO events_fts(rowid, content, summary)
            VALUES (new.id, new.content, coalesce(new.summary, ''));
        END;
        CREATE TRIGGER IF NOT EXISTS events_ad AFTER DELETE ON events BEGIN
            INSERT INTO events_fts(events_fts, rowid, content, summary)
            VALUES ('delete', old.id, old.content, coalesce(old.summary, ''));
        END;
        CREATE TRIGGER IF NOT EXISTS events_au AFTER UPDATE ON events BEGIN
            INSERT INTO events_fts(events_fts, rowid, content, summary)
            VALUES ('delete', old.id, old.content, coalesce(old.summary, ''));
            INSERT INTO events_fts(rowid, content, summary)
            VALUES (new.id, new.content, coalesce(new.summary, ''));
        END;
        "#,
    )?;
    Ok(())
}
