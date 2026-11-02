pub mod archive;
pub mod checksum;
pub mod error;
pub mod notice;
pub mod release;
pub mod replace;
pub mod target;
pub mod version;

pub use error::UpdateError;
pub use release::{Asset, Release};
pub use version::Version;
