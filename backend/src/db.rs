//! Database bootstrap for the backend service.
//!
//! Migrations are embedded into the binary so a deployment cannot accidentally
//! start an API against an uninitialized schema. The migration files remain
//! reviewable SQL under `backend/migrations`.

use sqlx::PgPool;

pub async fn run_migrations(pool: &PgPool) -> Result<(), sqlx::migrate::MigrateError> {
    sqlx::migrate!("./migrations").run(pool).await
}
