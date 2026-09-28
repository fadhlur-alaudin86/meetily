use crate::database::models::MeetingNotes;
use chrono::Utc;
use sqlx::SqlitePool;

/// Repository for the per-meeting user notes stored in `meeting_notes`.
pub struct NotesRepository;

impl NotesRepository {
    /// Returns the notes row for a meeting, if one has been created.
    pub async fn get(
        pool: &SqlitePool,
        meeting_id: &str,
    ) -> Result<Option<MeetingNotes>, sqlx::Error> {
        sqlx::query_as::<_, MeetingNotes>(
            "SELECT meeting_id, notes_markdown, notes_json, created_at, updated_at
             FROM meeting_notes WHERE meeting_id = ?",
        )
        .bind(meeting_id)
        .fetch_optional(pool)
        .await
    }

    /// Inserts or updates the notes for a meeting. An existing row keeps its
    /// `created_at`; only the content and `updated_at` change.
    pub async fn upsert(
        pool: &SqlitePool,
        meeting_id: &str,
        notes_markdown: Option<&str>,
        notes_json: Option<&str>,
    ) -> Result<MeetingNotes, sqlx::Error> {
        let now = Utc::now();
        sqlx::query(
            "INSERT INTO meeting_notes (meeting_id, notes_markdown, notes_json, created_at, updated_at)
             VALUES (?, ?, ?, ?, ?)
             ON CONFLICT(meeting_id) DO UPDATE SET
                notes_markdown = excluded.notes_markdown,
                notes_json = excluded.notes_json,
                updated_at = excluded.updated_at",
        )
        .bind(meeting_id)
        .bind(notes_markdown)
        .bind(notes_json)
        .bind(now)
        .bind(now)
        .execute(pool)
        .await?;

        // The row was just written; a missing row here would be a real error.
        Self::get(pool, meeting_id)
            .await
            .map(|row| row.expect("row must exist right after upsert"))
    }

    /// Removes the notes row. Returns true when a row was actually deleted.
    pub async fn delete(pool: &SqlitePool, meeting_id: &str) -> Result<bool, sqlx::Error> {
        let result = sqlx::query("DELETE FROM meeting_notes WHERE meeting_id = ?")
            .bind(meeting_id)
            .execute(pool)
            .await?;
        Ok(result.rows_affected() > 0)
    }

    /// Decides whether a save request means "clear the note":
    /// - markdown present but empty after trim -> the user emptied it -> clear.
    /// - markdown `None` (client-side conversion failed) -> keep whenever the
    ///   JSON carries any content, so a failed conversion never erases notes.
    /// - nothing at all -> nothing to store -> clear.
    pub fn is_clear_request(notes_markdown: Option<&str>, notes_json: Option<&str>) -> bool {
        match notes_markdown {
            Some(markdown) => markdown.trim().is_empty(),
            None => match notes_json {
                Some(json) => serde_json::from_str::<serde_json::Value>(json)
                    .map(|value| value.as_array().map_or(false, |blocks| blocks.is_empty()))
                    .unwrap_or_else(|_| json.trim().is_empty()),
                None => true,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn test_pool() -> SqlitePool {
        let pool = SqlitePool::connect("sqlite::memory:").await.unwrap();
        sqlx::query(
            "CREATE TABLE meeting_notes (
                meeting_id TEXT PRIMARY KEY NOT NULL,
                notes_markdown TEXT,
                notes_json TEXT,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL
            )",
        )
        .execute(&pool)
        .await
        .unwrap();
        pool
    }

    #[tokio::test]
    async fn test_get_returns_none_without_row() {
        let pool = test_pool().await;
        assert!(NotesRepository::get(&pool, "m1").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn test_upsert_creates_then_updates_preserving_created_at() {
        let pool = test_pool().await;

        let created = NotesRepository::upsert(&pool, "m1", Some("# first"), Some("[]"))
            .await
            .unwrap();
        assert_eq!(created.meeting_id, "m1");
        assert_eq!(created.notes_markdown.as_deref(), Some("# first"));

        std::thread::sleep(std::time::Duration::from_millis(20));
        let updated = NotesRepository::upsert(&pool, "m1", Some("# second"), Some("[{}]"))
            .await
            .unwrap();
        assert_eq!(updated.notes_markdown.as_deref(), Some("# second"));
        assert_eq!(
            updated.created_at, created.created_at,
            "created_at must be preserved across updates"
        );
        assert!(updated.updated_at >= created.updated_at);

        let rows: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM meeting_notes")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(rows.0, 1, "upsert must not duplicate rows");
    }

    #[tokio::test]
    async fn test_delete() {
        let pool = test_pool().await;

        assert!(
            !NotesRepository::delete(&pool, "m1").await.unwrap(),
            "deleting a missing row reports false"
        );

        NotesRepository::upsert(&pool, "m1", Some("x"), None)
            .await
            .unwrap();
        assert!(NotesRepository::delete(&pool, "m1").await.unwrap());
        assert!(NotesRepository::get(&pool, "m1").await.unwrap().is_none());
    }

    #[test]
    fn test_is_clear_request() {
        // Markdown present: empty after trim means clear, content means keep.
        assert!(NotesRepository::is_clear_request(Some(""), None));
        assert!(NotesRepository::is_clear_request(Some("   \n"), Some("[]")));
        assert!(!NotesRepository::is_clear_request(Some("hello"), None));

        // Markdown missing (conversion failed): keep whenever JSON has content.
        assert!(!NotesRepository::is_clear_request(
            None,
            Some("[{\"type\":\"paragraph\"}]")
        ));
        assert!(!NotesRepository::is_clear_request(None, Some("[{}]")));
        assert!(NotesRepository::is_clear_request(None, Some("[]")));
        assert!(NotesRepository::is_clear_request(None, Some("")));
        assert!(NotesRepository::is_clear_request(None, None));
    }
}
