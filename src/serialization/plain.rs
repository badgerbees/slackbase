use crate::serialization::Serializer;
use crate::types::Result;

pub struct PlainSerializer;

impl Serializer for PlainSerializer {
    fn serialize(&self, value: &str) -> Result<Vec<u8>> {
        Ok(value.as_bytes().to_vec())
    }
    fn deserialize(&self, data: &[u8]) -> Result<String> {
        String::from_utf8(data.to_vec()).map_err(|_| crate::types::Error::InvalidRecord)
    }
    fn box_clone(&self) -> Box<dyn Serializer> {
        Box::new(PlainSerializer)
    }
}