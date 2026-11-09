pub(crate) mod download;
pub(crate) mod extract;
pub(crate) mod index;

pub(crate) const RETRY_ATTEMPTS: usize = 3;
pub(crate) const RETRY_DELAY_MS: u64 = 100;
pub(crate) const DEFAULT_INDEX: &str = "https://index.crates.io";
