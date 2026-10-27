pub mod execute;
pub mod fixture;
pub mod import;
pub mod model;
pub mod native_import;
pub mod validate;
use sha2::{Digest, Sha256};
pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;
pub fn require(value: bool, message: &str) -> Result<()> {
    if value {
        Ok(())
    } else {
        Err(message.into())
    }
}
pub fn sha(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

pub mod volume_fixture;

mod native_clear;

pub mod compute_fixture;
pub mod compute_import;

pub mod depth_proxy_fixture;

pub mod graph_import;

pub mod finished_ops_fixture;
