use sqlx::SqlitePool;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum BlobError {
    #[error("database error: {0}")]
    Database(#[from] sqlx::Error),
    #[error("blob not found: {0}")]
    NotFound(String),
}

pub struct BlobStore {
    pool: SqlitePool,
}

pub struct StoredBlob {
    pub key: String,
    pub source: String,
    pub topic: String,
    pub mime: String,
    pub size: u64,
    pub payload: String,
    pub summary: String,
}

impl BlobStore {
    pub async fn connect(database_url: &str) -> Result<Self, BlobError> {
        let pool = SqlitePool::connect(database_url).await?;
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS context_blobs (
                key         TEXT PRIMARY KEY,
                source      TEXT NOT NULL,
                topic       TEXT NOT NULL,
                mime        TEXT NOT NULL DEFAULT 'application/json',
                size        INTEGER NOT NULL,
                payload     TEXT NOT NULL,
                summary     TEXT NOT NULL DEFAULT '',
                created_at  TEXT NOT NULL DEFAULT (datetime('now'))
            )",
        )
        .execute(&pool)
        .await?;
        sqlx::query(
            "CREATE INDEX IF NOT EXISTS idx_blobs_source_topic ON context_blobs(source, topic)",
        )
        .execute(&pool)
        .await?;
        Ok(Self { pool })
    }

    pub async fn insert(&self, blob: &StoredBlob) -> Result<(), BlobError> {
        sqlx::query(
            "INSERT OR REPLACE INTO context_blobs (key, source, topic, mime, size, payload, summary)
             VALUES (?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&blob.key)
        .bind(&blob.source)
        .bind(&blob.topic)
        .bind(&blob.mime)
        .bind(blob.size as i64)
        .bind(&blob.payload)
        .bind(&blob.summary)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn get(&self, key: &str) -> Result<StoredBlob, BlobError> {
        let row: Option<(String, String, String, String, i64, String, String)> = sqlx::query_as(
            "SELECT key, source, topic, mime, size, payload, summary FROM context_blobs WHERE key = ?",
        )
        .bind(key)
        .fetch_optional(&self.pool)
        .await?;
        match row {
            Some((key, source, topic, mime, size, payload, summary)) => Ok(StoredBlob {
                key,
                source,
                topic,
                mime,
                size: size as u64,
                payload,
                summary,
            }),
            None => Err(BlobError::NotFound(key.to_string())),
        }
    }

    pub async fn delete_older_than(&self, days: u32) -> Result<u64, BlobError> {
        let result = sqlx::query("DELETE FROM context_blobs WHERE created_at < datetime('now', ?)")
            .bind(format!("-{} days", days))
            .execute(&self.pool)
            .await?;
        Ok(result.rows_affected())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_blob_insert_and_get() {
        let store = BlobStore::connect("sqlite::memory:").await.unwrap();
        let blob = StoredBlob {
            key: "blob://test/audit/a1b2c3".to_string(),
            source: "test".to_string(),
            topic: "audit".to_string(),
            mime: "application/json".to_string(),
            size: 1024,
            payload: "{\"issues\": []}".to_string(),
            summary: "no issues".to_string(),
        };
        store.insert(&blob).await.unwrap();

        let retrieved = store.get("blob://test/audit/a1b2c3").await.unwrap();
        assert_eq!(retrieved.key, "blob://test/audit/a1b2c3");
        assert_eq!(retrieved.payload, "{\"issues\": []}");
        assert_eq!(retrieved.summary, "no issues");
    }

    #[tokio::test]
    async fn test_blob_get_not_found() {
        let store = BlobStore::connect("sqlite::memory:").await.unwrap();
        let result = store.get("nonexistent").await;
        assert!(matches!(result, Err(BlobError::NotFound(_))));
    }

    #[tokio::test]
    async fn test_blob_insert_or_replace() {
        let store = BlobStore::connect("sqlite::memory:").await.unwrap();
        let blob = StoredBlob {
            key: "blob://test/topic/hash".to_string(),
            source: "test".to_string(),
            topic: "topic".to_string(),
            mime: "application/json".to_string(),
            size: 100,
            payload: "original".to_string(),
            summary: "".to_string(),
        };
        store.insert(&blob).await.unwrap();

        let updated = StoredBlob {
            key: "blob://test/topic/hash".to_string(),
            source: "test".to_string(),
            topic: "topic".to_string(),
            mime: "application/json".to_string(),
            size: 200,
            payload: "updated".to_string(),
            summary: "v2".to_string(),
        };
        store.insert(&updated).await.unwrap();

        let retrieved = store.get("blob://test/topic/hash").await.unwrap();
        assert_eq!(retrieved.payload, "updated");
        assert_eq!(retrieved.summary, "v2");
    }

    #[tokio::test]
    async fn test_blob_delete_older_than() {
        let store = BlobStore::connect("sqlite::memory:").await.unwrap();
        // New blob should NOT be deleted by delete_older_than(7)
        let blob = StoredBlob {
            key: "blob://test/recent/hash".to_string(),
            source: "test".to_string(),
            topic: "recent".to_string(),
            mime: "application/json".to_string(),
            size: 50,
            payload: "data".to_string(),
            summary: "".to_string(),
        };
        store.insert(&blob).await.unwrap();
        let deleted = store.delete_older_than(7).await.unwrap();
        // Just inserted — should not be deleted
        assert_eq!(deleted, 0);
        // Still exists
        store.get("blob://test/recent/hash").await.unwrap();
    }
}
