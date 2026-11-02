use std::sync::atomic::{AtomicU32, Ordering};

static REQUESTS: AtomicU32 = AtomicU32::new(0);
static CACHE_HITS: AtomicU32 = AtomicU32::new(0);

pub fn request() {
    REQUESTS.fetch_add(1, Ordering::Relaxed);
}

pub fn cache_hit() {
    CACHE_HITS.fetch_add(1, Ordering::Relaxed);
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Counts {
    pub requests: u32,
    pub cache_hits: u32,
}

pub fn take() -> Counts {
    Counts {
        requests: REQUESTS.swap(0, Ordering::Relaxed),
        cache_hits: CACHE_HITS.swap(0, Ordering::Relaxed),
    }
}
