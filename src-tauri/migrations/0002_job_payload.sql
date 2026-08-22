-- M6: persist each job's kind-specific completion payload (extract batch
-- manifest / guides upsert data). Resume of a failed master_guide job needs the
-- enqueue-time payload even after an app restart, when the in-memory copy on
-- QueuedJob is gone.
ALTER TABLE jobs ADD COLUMN payload TEXT;
