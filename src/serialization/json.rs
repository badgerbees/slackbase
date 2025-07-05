use crate::types::Error;
use crate::types::Result;
use serde_json::{ Value, from_str, to_string };

pub struct JsonSerializer;

impl crate::serialization::Serializer for JsonSerializer {
    fn serialize(&self, value: &str) -> Result<Vec<u8>> {
        // Validate JSON
        let v: Value = from_str(value).map_err(Error::Serde)?;
        Ok(to_string(&v).map_err(Error::Serde)?.into_bytes())
    }
    fn deserialize(&self, data: &[u8]) -> Result<String> {
        String::from_utf8(data.to_vec()).map_err(|_| Error::InvalidRecord)
    }
    fn box_clone(&self) -> Box<dyn crate::serialization::Serializer> {
        Box::new(JsonSerializer)
    }
}
