pub mod challenge_store;
pub mod lease;
pub mod session_store;

pub use challenge_store::PgChallengeStore;
pub use lease::PgLeaseElector;
pub use session_store::PgSessionStore;

use diesel_async::AsyncPgConnection;
use diesel_async::pooled_connection::deadpool::{Object, Pool};

use crate::error::{GuardianError, Result};

/// Check out a pooled connection, mapping checkout failure to the fail-closed
/// `StorageError` surface. `context` labels the call site in the error message.
async fn checkout(
    pool: &Pool<AsyncPgConnection>,
    context: &str,
) -> Result<Object<AsyncPgConnection>> {
    pool.get()
        .await
        .map_err(|error| GuardianError::StorageError(format!("{context} pool: {error}")))
}
