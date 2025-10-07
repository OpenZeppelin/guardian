pub use private_state_manager_shared::{FromJson, ToJson};

#[tokio::main]
async fn main() {
    server::run().await;
}
