use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;
use tracing::info;

pub struct PoolRow {
    pub address: String,           // checksummed hex (or pool ID hex for V4)
    pub factory: String,           // checksummed hex
    pub asset0: String,            // checksummed hex
    pub asset1: String,            // checksummed hex
    pub creation_block: u64,
    pub fee: Option<i32>,
    pub tick_spacing: Option<i32>,
    pub additional_data: Option<serde_json::Value>,
}

pub struct PoolDb {
    pool: PgPool,
}

impl PoolDb {
    pub async fn new(database_url: &str) -> eyre::Result<Self> {
        let pool = PgPoolOptions::new()
            .max_connections(5)
            .connect(database_url)
            .await?;

        info!("Connected to PostgreSQL (network_1_dex_pools_cryo)");
        Ok(Self { pool })
    }

    /// Batch insert pools. Idempotent via ON CONFLICT DO NOTHING.
    pub async fn insert_pools(&self, pools: &[PoolRow]) -> eyre::Result<()> {
        if pools.is_empty() {
            return Ok(());
        }

        for chunk in pools.chunks(500) {
            let mut qb = sqlx::QueryBuilder::new(
                "INSERT INTO network_1_dex_pools_cryo (address, factory, asset0, asset1, creation_block, fee, tick_spacing, additional_data) ",
            );

            qb.push_values(chunk, |mut b, p| {
                b.push_bind(&p.address)
                    .push_bind(&p.factory)
                    .push_bind(&p.asset0)
                    .push_bind(&p.asset1)
                    .push_bind(p.creation_block as i32)
                    .push_bind(p.fee)
                    .push_bind(p.tick_spacing)
                    .push_bind(p.additional_data.as_ref().map(sqlx::types::Json));
            });

            qb.push(" ON CONFLICT (address) DO NOTHING");
            qb.build().execute(&self.pool).await?;
        }

        Ok(())
    }

    /// Delete all pools created in a specific block (reorg handling).
    pub async fn delete_block(&self, block_number: u64) -> eyre::Result<u64> {
        let result =
            sqlx::query("DELETE FROM network_1_dex_pools_cryo WHERE creation_block = $1")
                .bind(block_number as i32)
                .execute(&self.pool)
                .await?;
        Ok(result.rows_affected())
    }
}
