use anyhow::Result;
use sqlx::sqlite::SqlitePoolOptions;
use sqlx::{Pool, Sqlite as SqliteDb};
use sqlx::migrate::MigrateDatabase;

pub async fn create_pool(database_url: &str) -> Result<Pool<SqliteDb>> {
    if !SqliteDb::database_exists(database_url).await? {
        SqliteDb::create_database(database_url).await?;
    }
    let pool = SqlitePoolOptions::new()
        .max_connections(5)
        .connect(database_url)
        .await?;
    Ok(pool)
}

pub async fn run_migrations(pool: &Pool<SqliteDb>) -> Result<()> {
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS users (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            username TEXT NOT NULL UNIQUE,
            password_hash TEXT NOT NULL,
            api_key TEXT,
            created_at DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP
        )"
    )
    .execute(pool)
    .await?;

    sqlx::query(
        "CREATE TABLE IF NOT EXISTS translations (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            user_id INTEGER NOT NULL,
            filename TEXT NOT NULL,
            source_text TEXT NOT NULL,
            translated_text TEXT NOT NULL,
            source_lang TEXT NOT NULL DEFAULT 'en',
            target_lang TEXT NOT NULL DEFAULT 'zh',
            public_id TEXT NOT NULL,
            status TEXT NOT NULL DEFAULT 'done',
            created_at DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP,
            FOREIGN KEY (user_id) REFERENCES users(id)
        )"
    )
    .execute(pool)
    .await?;

    // 进程启动时，上次残留的"进行中"任务已经死掉了，重置为 failed 以免永久占锁
    let stuck = sqlx::query(
        "UPDATE translations SET status = 'failed' WHERE status IN ('running', 'splitting')",
    )
    .execute(pool)
    .await;
    if let Ok(r) = stuck {
        if r.rows_affected() > 0 {
            tracing::warn!("启动时重置了 {} 个中断的任务", r.rows_affected());
        }
    }

    // 老库补列：已存在时 SQLite 会报错，忽略即可
    let _ = sqlx::query("ALTER TABLE translations ADD COLUMN status TEXT NOT NULL DEFAULT 'done'")
        .execute(pool)
        .await;

    // 对外 ID：UUIDv7（时间有序，可排序）。老数据先补列，再回填，最后建唯一索引。
    let _ = sqlx::query("ALTER TABLE translations ADD COLUMN public_id TEXT")
        .execute(pool)
        .await;

    let missing: Vec<i64> =
        sqlx::query_scalar("SELECT id FROM translations WHERE public_id IS NULL OR public_id = ''")
            .fetch_all(pool)
            .await
            .unwrap_or_default();
    if !missing.is_empty() {
        let count = missing.len();
        for id in missing {
            let public_id = uuid::Uuid::now_v7().to_string();
            let _ = sqlx::query("UPDATE translations SET public_id = ? WHERE id = ?")
                .bind(&public_id)
                .bind(id)
                .execute(pool)
                .await;
        }
        tracing::info!("为 {} 条历史记录补充了 public_id", count);
    }

    let _ = sqlx::query(
        "CREATE UNIQUE INDEX IF NOT EXISTS idx_translations_public_id ON translations(public_id)",
    )
    .execute(pool)
    .await;

    Ok(())
}
