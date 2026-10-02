CREATE TABLE schema_migrations (
    version       INTEGER PRIMARY KEY CHECK (version > 0),
    name          TEXT NOT NULL,
    checksum      TEXT NOT NULL CHECK (
        length(checksum) = 64 AND checksum NOT GLOB '*[^0-9a-f]*'
    ),
    applied_at_ms INTEGER NOT NULL CHECK (applied_at_ms >= 0)
);

CREATE TABLE explicit_records (
    sequence      INTEGER PRIMARY KEY,
    event_id      TEXT NOT NULL UNIQUE CHECK (
        length(event_id) BETWEEN 1 AND 128
    ),
    record_kind   TEXT NOT NULL CHECK (
        record_kind IN ('preference', 'correction')
    ),
    record_key    TEXT NOT NULL CHECK (
        length(record_key) BETWEEN 1 AND 128
    ),
    record_value  TEXT NOT NULL CHECK (
        length(record_value) BETWEEN 1 AND 128
    ),
    created_at_ms INTEGER NOT NULL CHECK (created_at_ms >= 0)
);

CREATE INDEX explicit_records_lookup
    ON explicit_records(record_kind, record_key, sequence DESC);
