//! Pool e migração. SQLite num arquivo: ver decisão D-10 no repositório de pesquisa.

use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::SqlitePool;
use std::str::FromStr;

pub async fn abrir(url: &str) -> anyhow::Result<SqlitePool> {
    let opts = SqliteConnectOptions::from_str(url)?
        .create_if_missing(true)
        // O saldo é derivado de um ledger: WAL para o leitor do ranking não bloquear a
        // transação da aposta.
        .pragma("journal_mode", "WAL")
        .foreign_keys(true);
    let pool = SqlitePoolOptions::new()
        .max_connections(8)
        .connect_with(opts)
        .await?;
    sqlx::migrate!("./migrations").run(&pool).await?;
    Ok(pool)
}

pub fn agora() -> String {
    chrono::Utc::now().to_rfc3339()
}
