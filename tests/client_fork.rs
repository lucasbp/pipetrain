mod common;

use common::{canned, serve};
use pipetrain::config::ApiConfig;
use pipetrain::github::{ApiError, Client};

#[test]
fn fork_keeps_transport_and_shares_global_request_limit() {
    let (base, server) = serve(vec![canned("200 OK", "{}")]);
    let cfg = ApiConfig {
        max_requests: 1,
        max_per_minute: 1000,
        min_remaining: 0,
    };
    let mut original = Client::with_base_url("ghp_FAKE0123456789".to_string(), &cfg, false, &base);
    let mut worker = original.fork();
    assert_eq!(worker.get("/repos/acme/api").unwrap().status, 200);
    assert_eq!(original.requests(), 1);
    assert!(matches!(
        original.get("/repos/acme/api"),
        Err(ApiError::Stopped { .. })
    ));
    assert_eq!(server.join().unwrap().len(), 1);
}
