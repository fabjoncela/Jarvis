ALTER TABLE explicit_records RENAME TO explicit_records_v1;

DROP INDEX explicit_records_lookup;

CREATE TABLE explicit_records (
    sequence        INTEGER PRIMARY KEY,
    event_id        TEXT NOT NULL UNIQUE CHECK (
        length(event_id) BETWEEN 1 AND 128
    ),
    record_kind     TEXT NOT NULL CHECK (
        record_kind IN (
            'preference', 'preference_forget', 'preference_restore', 'correction'
        )
    ),
    record_key      TEXT NOT NULL CHECK (
        length(record_key) BETWEEN 1 AND 128
    ),
    record_value    TEXT NOT NULL CHECK (
        length(record_value) BETWEEN 1 AND 128
    ),
    causal_event_id TEXT CHECK (
        causal_event_id IS NULL OR length(causal_event_id) BETWEEN 1 AND 128
    ),
    created_at_ms   INTEGER NOT NULL CHECK (created_at_ms >= 0),
    CHECK (
        (record_kind IN ('preference', 'correction') AND causal_event_id IS NULL)
        OR (record_kind IN ('preference_forget', 'preference_restore')
            AND causal_event_id IS NOT NULL)
    )
);

INSERT INTO explicit_records (
    sequence, event_id, record_kind, record_key, record_value, created_at_ms
)
SELECT sequence, event_id, record_kind, record_key, record_value, created_at_ms
FROM explicit_records_v1;

DROP TABLE explicit_records_v1;

CREATE INDEX explicit_records_lookup
    ON explicit_records(record_kind, record_key, sequence DESC);

CREATE INDEX explicit_preference_state
    ON explicit_records(record_key, sequence DESC)
    WHERE record_kind IN ('preference', 'preference_forget', 'preference_restore');
