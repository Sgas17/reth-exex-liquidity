use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;
use std::time::Duration;
use tracing::info;

pub struct TransferRow {
    pub block_number: u64,
    pub tx_hash: String,
    pub log_index: u32,
    pub token_address: String,
    pub from_address: String,
    pub to_address: String,
    pub amount_str: String, // U256.to_string() decimal representation
    pub block_timestamp: u64,
}

pub struct TransferDb {
    pool: PgPool,
}

impl TransferDb {
    pub async fn new(database_url: &str) -> eyre::Result<Self> {
        let pool = PgPoolOptions::new()
            .max_connections(20)
            .min_connections(2)
            .acquire_timeout(Duration::from_secs(60))
            .idle_timeout(Duration::from_secs(300))
            .max_lifetime(Duration::from_secs(1800))
            .connect(database_url)
            .await?;

        let db = Self { pool };
        db.init_schema().await?;
        Ok(db)
    }

    async fn init_schema(&self) -> eyre::Result<()> {
        // Migration: drop old BYTEA-based tables if they exist
        sqlx::query(
            r#"
            DO $$
            BEGIN
                -- Check if erc20_transfers exists with BYTEA columns (old schema)
                IF EXISTS (
                    SELECT 1 FROM information_schema.columns
                    WHERE table_name = 'erc20_transfers'
                      AND column_name = 'tx_hash'
                      AND data_type = 'bytea'
                ) THEN
                    DROP MATERIALIZED VIEW IF EXISTS top_transferred_tokens;
                    DROP TABLE IF EXISTS token_transfer_stats;
                    DROP TABLE IF EXISTS erc20_transfers;
                    RAISE NOTICE 'Dropped old BYTEA-based tables';
                END IF;
            END
            $$
            "#,
        )
        .execute(&self.pool)
        .await?;

        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS erc20_transfers (
                block_number    BIGINT NOT NULL,
                tx_hash         TEXT NOT NULL,
                log_index       INTEGER NOT NULL,
                token_address   TEXT NOT NULL,
                from_address    TEXT NOT NULL,
                to_address      TEXT NOT NULL,
                amount          NUMERIC NOT NULL,
                block_timestamp BIGINT NOT NULL,
                CONSTRAINT erc20_transfers_pkey PRIMARY KEY (tx_hash, log_index)
            )
            "#,
        )
        .execute(&self.pool)
        .await?;

        sqlx::query(
            "CREATE INDEX IF NOT EXISTS idx_transfers_block_timestamp ON erc20_transfers (block_timestamp)",
        )
        .execute(&self.pool)
        .await?;

        sqlx::query(
            "CREATE INDEX IF NOT EXISTS idx_transfers_block_number ON erc20_transfers (block_number)",
        )
        .execute(&self.pool)
        .await?;

        sqlx::query(
            "CREATE INDEX IF NOT EXISTS idx_transfers_token_timestamp ON erc20_transfers (token_address, block_timestamp)",
        )
        .execute(&self.pool)
        .await?;

        // Token metadata — populated by an external service (price feed)
        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS token_metadata (
                token_address   TEXT PRIMARY KEY,
                symbol          TEXT,
                decimals        INTEGER NOT NULL DEFAULT 18,
                price_usd       DOUBLE PRECISION NOT NULL DEFAULT 0,
                market_cap_usd  DOUBLE PRECISION NOT NULL DEFAULT 0,
                updated_at      BIGINT NOT NULL DEFAULT 0
            )
            "#,
        )
        .execute(&self.pool)
        .await?;

        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS token_transfer_stats (
                token_address           TEXT PRIMARY KEY,
                transfer_count_24h      BIGINT NOT NULL DEFAULT 0,
                transfer_count_7d       BIGINT NOT NULL DEFAULT 0,
                unique_senders_24h      BIGINT NOT NULL DEFAULT 0,
                unique_senders_7d       BIGINT NOT NULL DEFAULT 0,
                unique_receivers_24h    BIGINT NOT NULL DEFAULT 0,
                unique_receivers_7d     BIGINT NOT NULL DEFAULT 0,
                volume_usd_24h          DOUBLE PRECISION NOT NULL DEFAULT 0,
                volume_usd_7d           DOUBLE PRECISION NOT NULL DEFAULT 0,
                volume_mcap_ratio_24h   DOUBLE PRECISION NOT NULL DEFAULT 0,
                volume_mcap_ratio_7d    DOUBLE PRECISION NOT NULL DEFAULT 0,
                ranking_score           DOUBLE PRECISION NOT NULL DEFAULT 0,
                updated_at              BIGINT NOT NULL DEFAULT 0
            )
            "#,
        )
        .execute(&self.pool)
        .await?;

        sqlx::query(
            "CREATE INDEX IF NOT EXISTS idx_token_stats_ranking ON token_transfer_stats (ranking_score DESC)",
        )
        .execute(&self.pool)
        .await?;

        sqlx::query(
            r#"
            DO $$
            BEGIN
                IF NOT EXISTS (
                    SELECT 1 FROM pg_matviews WHERE matviewname = 'top_transferred_tokens'
                ) THEN
                    EXECUTE '
                        CREATE MATERIALIZED VIEW top_transferred_tokens AS
                        SELECT * FROM token_transfer_stats
                        WHERE ranking_score > 0
                        ORDER BY ranking_score DESC
                        LIMIT 500
                    ';
                    EXECUTE '
                        CREATE UNIQUE INDEX idx_top_tokens_address
                        ON top_transferred_tokens (token_address)
                    ';
                END IF;
            END
            $$
            "#,
        )
        .execute(&self.pool)
        .await?;

        info!("Database schema initialized");
        Ok(())
    }

    /// Batch insert transfers for a block. Idempotent via ON CONFLICT DO NOTHING.
    pub async fn insert_transfers(&self, transfers: &[TransferRow]) -> eyre::Result<()> {
        if transfers.is_empty() {
            return Ok(());
        }

        // Chunk to stay under Postgres parameter limits (65535 params / 8 cols ≈ 8191 rows)
        for chunk in transfers.chunks(1000) {
            let mut qb = sqlx::QueryBuilder::new(
                "INSERT INTO erc20_transfers (block_number, tx_hash, log_index, token_address, from_address, to_address, amount, block_timestamp) ",
            );

            qb.push_values(chunk, |mut b, t| {
                b.push_bind(t.block_number as i64)
                    .push_bind(&t.tx_hash)
                    .push_bind(t.log_index as i32)
                    .push_bind(&t.token_address)
                    .push_bind(&t.from_address)
                    .push_bind(&t.to_address)
                    .push_bind(&t.amount_str)
                    .push_unseparated("::NUMERIC")
                    .push_bind(t.block_timestamp as i64);
            });

            qb.push(" ON CONFLICT (tx_hash, log_index) DO NOTHING");
            qb.build().execute(&self.pool).await?;
        }

        Ok(())
    }

    /// Delete all transfers for a block (reorg handling).
    pub async fn delete_block(&self, block_number: u64) -> eyre::Result<u64> {
        let result = sqlx::query("DELETE FROM erc20_transfers WHERE block_number = $1")
            .bind(block_number as i64)
            .execute(&self.pool)
            .await?;
        Ok(result.rows_affected())
    }

    /// Aggregate token stats, join against token_metadata for USD volume and mcap ratio.
    ///
    /// Ranking score:
    ///   transfer_count_24h * 0.3
    /// + unique_senders_24h * 0.15
    /// + unique_receivers_24h * 0.15
    /// + volume_mcap_ratio_24h * 1000 * 0.2   (scaled up since ratios are small)
    /// + transfer_count_7d * 0.1
    /// + unique_senders_7d * 0.05
    /// + unique_receivers_7d * 0.05
    pub async fn run_aggregation(&self) -> eyre::Result<()> {
        let now_ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_secs() as i64;
        let ts_24h = now_ts - 86400;
        let ts_7d = now_ts - 604800;

        sqlx::query(
            r#"
            INSERT INTO token_transfer_stats (
                token_address,
                transfer_count_24h, transfer_count_7d,
                unique_senders_24h, unique_senders_7d,
                unique_receivers_24h, unique_receivers_7d,
                volume_usd_24h, volume_usd_7d,
                volume_mcap_ratio_24h, volume_mcap_ratio_7d,
                ranking_score, updated_at
            )
            SELECT
                t.token_address,
                COUNT(*) FILTER (WHERE t.block_timestamp >= $1),
                COUNT(*),
                COUNT(DISTINCT t.from_address) FILTER (WHERE t.block_timestamp >= $1),
                COUNT(DISTINCT t.from_address),
                COUNT(DISTINCT t.to_address) FILTER (WHERE t.block_timestamp >= $1),
                COUNT(DISTINCT t.to_address),
                -- volume_usd: raw_amount / 10^decimals * price_usd
                COALESCE(SUM(t.amount / pow(10, COALESCE(m.decimals, 18)) * COALESCE(m.price_usd, 0))
                    FILTER (WHERE t.block_timestamp >= $1), 0),
                COALESCE(SUM(t.amount / pow(10, COALESCE(m.decimals, 18)) * COALESCE(m.price_usd, 0)), 0),
                -- volume_mcap_ratio: volume_usd / market_cap (0 if no mcap data)
                CASE WHEN COALESCE(m.market_cap_usd, 0) > 0
                    THEN COALESCE(SUM(t.amount / pow(10, COALESCE(m.decimals, 18)) * COALESCE(m.price_usd, 0))
                        FILTER (WHERE t.block_timestamp >= $1), 0) / m.market_cap_usd
                    ELSE 0
                END,
                CASE WHEN COALESCE(m.market_cap_usd, 0) > 0
                    THEN COALESCE(SUM(t.amount / pow(10, COALESCE(m.decimals, 18)) * COALESCE(m.price_usd, 0)), 0)
                        / m.market_cap_usd
                    ELSE 0
                END,
                -- ranking_score
                (COUNT(*) FILTER (WHERE t.block_timestamp >= $1) * 0.3 +
                 COUNT(DISTINCT t.from_address) FILTER (WHERE t.block_timestamp >= $1) * 0.15 +
                 COUNT(DISTINCT t.to_address) FILTER (WHERE t.block_timestamp >= $1) * 0.15 +
                 CASE WHEN COALESCE(m.market_cap_usd, 0) > 0
                     THEN COALESCE(SUM(t.amount / pow(10, COALESCE(m.decimals, 18)) * COALESCE(m.price_usd, 0))
                         FILTER (WHERE t.block_timestamp >= $1), 0) / m.market_cap_usd * 1000 * 0.2
                     ELSE 0
                 END +
                 COUNT(*) * 0.1 +
                 COUNT(DISTINCT t.from_address) * 0.05 +
                 COUNT(DISTINCT t.to_address) * 0.05),
                $3
            FROM erc20_transfers t
            LEFT JOIN token_metadata m ON t.token_address = m.token_address
            WHERE t.block_timestamp >= $2
            GROUP BY t.token_address, m.decimals, m.price_usd, m.market_cap_usd
            ON CONFLICT (token_address)
            DO UPDATE SET
                transfer_count_24h = EXCLUDED.transfer_count_24h,
                transfer_count_7d = EXCLUDED.transfer_count_7d,
                unique_senders_24h = EXCLUDED.unique_senders_24h,
                unique_senders_7d = EXCLUDED.unique_senders_7d,
                unique_receivers_24h = EXCLUDED.unique_receivers_24h,
                unique_receivers_7d = EXCLUDED.unique_receivers_7d,
                volume_usd_24h = EXCLUDED.volume_usd_24h,
                volume_usd_7d = EXCLUDED.volume_usd_7d,
                volume_mcap_ratio_24h = EXCLUDED.volume_mcap_ratio_24h,
                volume_mcap_ratio_7d = EXCLUDED.volume_mcap_ratio_7d,
                ranking_score = EXCLUDED.ranking_score,
                updated_at = EXCLUDED.updated_at
            "#,
        )
        .bind(ts_24h)
        .bind(ts_7d)
        .bind(now_ts)
        .execute(&self.pool)
        .await?;

        // Refresh materialized view (CONCURRENTLY requires the unique index)
        sqlx::query("REFRESH MATERIALIZED VIEW CONCURRENTLY top_transferred_tokens")
            .execute(&self.pool)
            .await?;

        Ok(())
    }

    /// Delete transfers older than 7 days.
    pub async fn cleanup_old_transfers(&self) -> eyre::Result<u64> {
        let cutoff = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_secs() as i64
            - 604800;

        let result = sqlx::query("DELETE FROM erc20_transfers WHERE block_timestamp < $1")
            .bind(cutoff)
            .execute(&self.pool)
            .await?;
        Ok(result.rows_affected())
    }
}
