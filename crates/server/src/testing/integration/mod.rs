// Integration tests
#[cfg(test)]
mod e2e_http_auth;

#[cfg(test)]
mod e2e_grpc_auth;

#[cfg(test)]
mod canonicalization_lifecycle;

#[cfg(test)]
mod miden_rpc_integration;

#[cfg(test)]
mod generate_fixtures;
