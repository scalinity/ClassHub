-- Indexes on the columns the hot queries actually filter by. At a semester's
-- scale these are microsecond scans either way; chat_messages(session_id) is
-- the one that earns its keep, since rebuilding the request re-reads a whole
-- session once per round of the tool loop.
CREATE INDEX IF NOT EXISTS idx_deadlines_class ON deadlines(class_id);
CREATE INDEX IF NOT EXISTS idx_chat_messages_session ON chat_messages(session_id);
CREATE INDEX IF NOT EXISTS idx_move_proposals_class_status ON move_proposals(class_id, status);
CREATE INDEX IF NOT EXISTS idx_deadline_proposals_class_status ON deadline_proposals(class_id, status);
CREATE INDEX IF NOT EXISTS idx_jobs_kind_class_status ON jobs(kind, class_id, status);
