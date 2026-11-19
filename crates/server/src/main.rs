pub use guardian_shared::{FromJson, ToJson};

use server::builder::ServerBuilder;

#[tokio::main]
async fn main() {
    dotenvy::dotenv().ok();

    ServerBuilder::from_env()
        .await
        .unwrap_or_else(|error| panic!("Failed to configure server: {error}"))
        .build()
        .await
        .unwrap_or_else(|error| panic!("Failed to build server: {error}"))
        .run()
        .await;
}
