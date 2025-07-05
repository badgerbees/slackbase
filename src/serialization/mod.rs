pub mod plain;
pub mod json;

use crate::types::Result;

pub trait Serializer: Send + Sync {
    fn serialize(&self, value: &str) -> Result<Vec<u8>>;
    fn deserialize(&self, data: &[u8]) -> Result<String>;
    fn box_clone(&self) -> Box<dyn Serializer>;
}

