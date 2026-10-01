-- Counts resets of an account's sync. A device that only looks (no push socket) learns of a reset
-- from the generation in a pull changing, where a pushed device gets the Reset nudge.
ALTER TABLE sync_heads ADD COLUMN generation bigint NOT NULL DEFAULT 0;
