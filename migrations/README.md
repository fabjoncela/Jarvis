# Database migrations

Numbered, transactional SQLite migrations will be added with the storage
milestone. Runtime databases belong under `%LOCALAPPDATA%\Jarvis`, not here.
Every migration must have an idempotency and recovery test before use.
