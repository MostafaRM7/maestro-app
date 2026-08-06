ALTER TABLE raw_segments ADD COLUMN run_id TEXT REFERENCES process_runs(id) ON DELETE CASCADE;
ALTER TABLE raw_segments ADD COLUMN content BLOB NOT NULL DEFAULT X'';
ALTER TABLE raw_segments ADD COLUMN observed_byte_count INTEGER NOT NULL DEFAULT 0;
ALTER TABLE raw_segments ADD COLUMN truncated INTEGER NOT NULL DEFAULT 0;
ALTER TABLE raw_segments ADD COLUMN completed INTEGER NOT NULL DEFAULT 0;

CREATE UNIQUE INDEX raw_segments_run_idx
ON raw_segments(run_id)
WHERE run_id IS NOT NULL;
