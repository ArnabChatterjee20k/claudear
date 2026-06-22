CREATE TABLE IF NOT EXISTS discord_message_chunks (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    guild_id TEXT,
    channel_id TEXT NOT NULL,
    thread_id TEXT,

    start_message_id TEXT NOT NULL,
    end_message_id TEXT NOT NULL,
    participant_ids TEXT,

    start_message_time TEXT NOT NULL,
    end_message_time TEXT NOT NULL,

    chunk_text TEXT NOT NULL,
    context_text TEXT NOT NULL,
    content_hash TEXT
);
CREATE INDEX IF NOT EXISTS idx_discord_message_chunks_channel ON discord_message_chunks(channel_id);
CREATE INDEX IF NOT EXISTS idx_discord_message_chunks_content_hash ON discord_message_chunks(content_hash);

CREATE TABLE IF NOT EXISTS discord_message_chunk_embeddings (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    chunk_id INTEGER NOT NULL UNIQUE REFERENCES discord_message_chunks(id) ON DELETE CASCADE,
    embedding BLOB NOT NULL,
    embedding_model TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS discord_message_chunk_metadata (
    key TEXT NOT NULL,
    value TEXT NOT NULL,
    PRIMARY KEY (key)
);
