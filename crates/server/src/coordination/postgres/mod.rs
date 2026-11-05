pub mod block_watch_store;
pub mod challenge_store;
pub mod lease;
pub mod read_records;
pub mod session_store;
pub mod stats_store;

pub use block_watch_store::PgBlockWatchStore;
pub use challenge_store::PgChallengeStore;
pub use lease::PgLeaseElector;
pub use read_records::PgReadRecords;
pub use session_store::PgSessionStore;
pub use stats_store::PgStatsStore;

use diesel_async::AsyncPgConnection;
use diesel_async::pooled_connection::deadpool::{Object, Pool};

use crate::error::{GuardianError, Result};
use crate::metrics::labels::PoolKind;
use crate::storage::postgres::TimedCheckout;

/// Check out a pooled connection, mapping checkout failure to the fail-closed
/// `StorageError` surface. `context` labels the call site in the error message.
async fn checkout(
    pool: &Pool<AsyncPgConnection>,
    context: &str,
) -> Result<Object<AsyncPgConnection>> {
    pool.get_timed(PoolKind::Metadata)
        .await
        .map_err(|error| GuardianError::StorageError(format!("{context} pool: {error}")))
}
