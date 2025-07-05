use std::collections::{ HashMap, HashSet };
use serde::{Serialize, Deserialize};

#[derive(Serialize, Deserialize, Debug, Default)]
pub struct SecondaryIndex {
    // field => value => set of keys
    index: HashMap<String, HashMap<String, HashSet<String>>>,
}

impl SecondaryIndex {
    pub fn new() -> Self {
        Self { index: HashMap::new() }
    }

    pub fn clear(&mut self) {
        self.index.clear();
    }

    /// Called on put/putex or json_set_field (with old+new JSON!).
    pub fn update(&mut self, key: &str, old_json: Option<&str>, new_json: Option<&str>) {
        // Remove all previous values for this key if old_json is given
        if let Some(s) = old_json {
            if let Ok(val) = serde_json::from_str::<serde_json::Value>(s) {
                if let Some(map) = val.as_object() {
                    for (field, v) in map {
                        let strval: String = match v {
                            serde_json::Value::String(s) => s.clone(),
                            _ => v.to_string(),
                        };
                        if let Some(valmap) = self.index.get_mut(field) {
                            if let Some(set) = valmap.get_mut(&strval) {
                                set.remove(key);
                                // Clean up empty sets and maps
                                if set.is_empty() {
                                    valmap.remove(&strval);
                                }
                            }
                            if valmap.is_empty() {
                                self.index.remove(field);
                            }
                        }
                    }
                }
            }
        }

        // Add new values for this key if new_json is given
        if let Some(s) = new_json {
            if let Ok(val) = serde_json::from_str::<serde_json::Value>(s) {
                if let Some(map) = val.as_object() {
                    for (field, v) in map {
                        let strval: String = match v {
                            serde_json::Value::String(s) => s.clone(),
                            _ => v.to_string(),
                        };
                        self.index
                            .entry(field.clone())
                            .or_default()
                            .entry(strval)
                            .or_default()
                            .insert(key.to_string());
                    }
                }
            }
        }
    }

    /// Called on delete.
    pub fn remove(&mut self, key: &str, old_json: Option<&str>) {
        self.update(key, old_json, None);
    }

    pub fn find(&self, field: &str, value: &str) -> Vec<String> {
        self.index
            .get(field)
            .and_then(|m| m.get(value))
            .map(|set| set.iter().cloned().collect())
            .unwrap_or_default()
    }
}
