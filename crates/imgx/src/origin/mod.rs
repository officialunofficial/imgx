pub mod fetcher;
pub mod r2;
pub mod remote;
pub mod source;

pub use fetcher::{FetchError, FetchResult, Fetcher};
pub use r2::R2Fetcher;
pub use remote::{RemoteFetchError, RemoteFetchResult, RemoteFetcher};
pub use source::OriginSource;
