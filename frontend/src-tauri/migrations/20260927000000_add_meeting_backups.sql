-- Migration: Add meeting_backups table to track backup status and paths
CREATE TABLE IF NOT EXISTS meeting_backups (
    meeting_id TEXT PRIMARY KEY NOT NULL,
    backup_path TEXT NOT NULL,
    status TEXT NOT NULL, -- 'ok', 'partial', 'failed'
    backed_up_at TEXT NOT NULL,
    has_audio INTEGER NOT NULL DEFAULT 1,
    has_summary INTEGER NOT NULL DEFAULT 0,
    FOREIGN KEY (meeting_id) REFERENCES meetings(id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_meeting_backups_status ON meeting_backups(status);
