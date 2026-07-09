pub mod fetcher;
pub mod r2;
pub mod source;

pub use fetcher::{FetchError, FetchResult, Fetcher};
pub use r2::R2Fetcher;
pub use source::OriginSource;
