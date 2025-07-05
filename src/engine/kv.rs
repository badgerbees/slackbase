use crate::storage::file as storage;
use crate::engine::wal::WAL;
use crate::types::{ Result, Error };
use std::collections::HashMap;
use std::fs::{ self, OpenOptions };
use std::io::{ self, Write };
use crate::serialization::Serializer;
use base64::{ engine::general_purpose, Engine };
use crate::engine::batch::BatchOp;
use std::time::{ SystemTime, UNIX_EPOCH };
use crate::types::ScriptMeta;
use crate::engine::index::SecondaryIndex;
use lru::LruCache;

// For Lua scripting support
use mlua::{ Lua, Function, Value };
use sha1::{ Sha1, Digest };
use hex;
use serde_json;

pub struct SlackbaseEngine {
    db_path: String,
    index: HashMap<String, (u64, usize)>,
    pub sec_index: SecondaryIndex,
    wal: WAL,
    write_buffer: Vec<String>,
    serializer: Box<dyn Serializer>,
    pub lru: LruCache<String, String>,

    pub read_ops: usize,
    pub write_ops: usize,
    pub hits: usize,
    pub misses: usize,

    lua: Lua,
    pub scripts: HashMap<String, Function<'static>>,
    pub script_meta: HashMap<String, ScriptMeta>, // sha1 → meta
    pub script_names: HashMap<String, String>, // name → sha1
}

impl SlackbaseEngine {
    /// Opens the database, recovers from WAL, and loads scripts.
    pub fn open(db_path: &str, serializer: Box<dyn Serializer>) -> Result<Self> {
        let wal = WAL::open(&format!("{}.wal", db_path))?;
        let use_hint = {
            let hint_meta = fs::metadata(&format!("{}.hint", db_path)).ok();
            let db_meta = fs::metadata(db_path).ok();
            match (db_meta, hint_meta) {
                (Some(db), Some(hint)) => hint.modified().ok() >= db.modified().ok(),
                _ => false,
            }
        };
        let index = if use_hint {
            storage::load_hint(db_path)?
        } else {
            let idx = storage::build_offset_index(db_path)?;
            let _ = storage::save_hint(db_path, &idx);
            idx
        };

        let lua = Lua::new();
        let scripts = HashMap::new();
        let script_meta = HashMap::new();
        let script_names = HashMap::new();
        let sec_index = {
            let path = format!("{}.secindex", db_path);
            if let Ok(data) = std::fs::read(&path) {
                serde_json::from_slice(&data).unwrap_or_else(|_| SecondaryIndex::new())
            } else {
                SecondaryIndex::new()
            }
        };
        let lru = LruCache::new(std::num::NonZeroUsize::new(1024).unwrap());

        let mut engine = Self {
            db_path: db_path.to_string(),
            index,
            sec_index,
            wal,
            write_buffer: Vec::new(),
            serializer,
            lru,
            read_ops: 0,
            write_ops: 0,
            hits: 0,
            misses: 0,
            lua,
            scripts,
            script_meta,
            script_names,
        };

        engine.recover_from_wal()?;
        engine.load_scripts_from_disk()?;

        Ok(engine)
    }

    /// Flushes all buffered writes to WAL.
    fn flush_buffer(&mut self) -> Result<()> {
        for record in &self.write_buffer {
            self.wal.append(record)?;
        }
        self.write_buffer.clear();
        Ok(())
    }

    /// Internal put logic supporting TTL.
    fn put_internal(&mut self, key: &str, value: &str, expires_at: Option<u64>) -> Result<()> {
        self.write_ops += 1;

        // --- Secondary index update
        let old_val = self.get(key);
        let old_json = old_val.as_deref();
        let new_json = Some(value);
        self.sec_index.update(key, old_json, new_json);
        self.save_sec_index().ok();

        // --- Serialize value ---
        let encoded = self.serializer.serialize(value)?;
        let encoded_str = general_purpose::STANDARD.encode(&encoded);
        let record = match expires_at {
            Some(ts) => format!("put\t{}\t{}\t{}", key, encoded_str, ts),
            None => format!("put\t{}\t{}\t", key, encoded_str),
        };
        // --- Write to WAL and buffer
        self.write_buffer.push(record.clone());
        self.flush_buffer()?;
        let (offset, len) = storage::append_record(&self.db_path, &record)?;
        self.index.insert(key.to_string(), (offset, len));
        storage::save_hint(&self.db_path, &self.index)?;

        // --- LRU cache: insert or update ---
        self.lru.put(key.to_string(), value.to_string());

        Ok(())
    }

    /// Puts a key-value pair.
    pub fn put(&mut self, key: &str, value: &str) -> Result<()> {
        self.put_internal(key, value, None)
    }

    /// Puts a key-value pair with TTL.
    pub fn putex(&mut self, key: &str, value: &str, ttl_secs: u64) -> Result<()> {
        let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
        self.put_internal(key, value, Some(now + ttl_secs))
    }

    /// Gets a value by key.
    pub fn get(&mut self, key: &str) -> Option<String> {
        self.read_ops += 1;
        // 1. Fast path: check LRU cache first
        if let Some(val) = self.lru.get(key) {
            self.hits += 1;
            return Some(val.clone());
        }

        // 2. Fall back to disk/index
        let (offset, len) = *self.index.get(key)?;
        let raw = storage::read_record_slice(&self.db_path, offset, len).ok().flatten()?;
        let parts: Vec<&str> = raw.split('\t').collect();

        if parts.len() < 3 || parts[0] != "put" {
            self.misses += 1;
            return None;
        }

        let encoded_str = parts[2];
        if parts.len() >= 4 && !parts[3].is_empty() {
            let expires_at: u64 = parts[3].parse().ok()?;
            if SystemTime::now().duration_since(UNIX_EPOCH).ok()?.as_secs() > expires_at {
                self.misses += 1;
                return None;
            }
        }

        let bytes = general_purpose::STANDARD.decode(encoded_str).ok()?;
        let value = self.serializer.deserialize(&bytes).ok()?;

        // 3. Store in LRU cache for future fast lookup
        self.lru.put(key.to_string(), value.clone());
        self.hits += 1;
        Some(value)
    }

    /// Deletes a key.
    pub fn delete(&mut self, key: &str) -> Result<()> {
        self.write_ops += 1;
        // 1. Get the old value BEFORE removal for index update!
        let old_val = self.get(key);

        let record = format!("del\t{}", key);
        self.write_buffer.push(record.clone());
        self.flush_buffer()?;
        let (_off, _len) = storage::append_record(&self.db_path, &record)?;
        self.index.remove(key);

        // 2. Update the secondary index
        self.sec_index.remove(key, old_val.as_deref());
        self.save_sec_index().ok(); // <-- persist index after delete

        // --- LRU cache: remove deleted key ---
        self.lru.pop(key);

        storage::save_hint(&self.db_path, &self.index)?;
        Ok(())
    }

    /// Compacts the database log and reindexes.
    pub fn compact(&mut self) -> Result<()> {
        self.flush_buffer()?;
        storage::compact_log(&self.db_path)?;

        self.index = storage::build_offset_index(&self.db_path)?;
        storage::save_hint(&self.db_path, &self.index)?;

        self.wal.clear()?;
        *self = SlackbaseEngine::open(&self.db_path, self.serializer.box_clone())?;
        Ok(())
    }

    /// Executes a batch of operations atomically.
    pub fn batch(&mut self, ops: Vec<BatchOp>) -> Result<()> {
        self.flush_buffer()?;
        self.wal.append("BEGIN")?;
        for op in &ops {
            match op {
                BatchOp::Put(k, v) => self.wal.append(&format!("put\t{}\t{}", k, v))?,
                BatchOp::Del(k) => self.wal.append(&format!("del\t{}", k))?,
            }
        }
        self.wal.append("END")?;
        self.wal.flush()?;
        for op in ops {
            match op {
                BatchOp::Put(k, v) => self.put(&k, &v)?,
                BatchOp::Del(k) => self.delete(&k)?,
            }
        }
        Ok(())
    }

    /// Recovers completed batches from WAL on startup.
    fn recover_from_wal(&mut self) -> Result<()> {
        let entries = self.wal.iter()?; // Expects WAL to provide all lines as Vec<String>
        let mut in_tx = false;
        let mut batch = Vec::<String>::new();

        for entry in entries {
            match entry.as_str() {
                "BEGIN" => {
                    in_tx = true;
                    batch.clear();
                }
                "END" if in_tx => {
                    for op in &batch {
                        if op.starts_with("put\t") {
                            let parts: Vec<&str> = op.splitn(3, '\t').collect();
                            if parts.len() == 3 {
                                self.put(parts[1], parts[2])?;
                            }
                        } else if op.starts_with("del\t") {
                            let parts: Vec<&str> = op.splitn(2, '\t').collect();
                            if parts.len() == 2 {
                                self.delete(parts[1])?;
                            }
                        }
                    }
                    in_tx = false;
                    batch.clear();
                }
                _ if in_tx => batch.push(entry.clone()),
                _ => {}
            }
        }
        Ok(())
    }

    /// Saves script metadata and source to disk.
    pub fn save_scripts_to_disk(&self) -> Result<()> {
        let meta_path = format!("{}.scripts", self.db_path);
        let script_list: Vec<ScriptMeta> = self.script_meta.values().cloned().collect();
        let serialized = serde_json::to_string_pretty(&script_list)?;
        fs::write(meta_path, serialized)?;
        Ok(())
    }

    /// Loads script metadata and source from disk.
    pub fn load_scripts_from_disk(&mut self) -> Result<()> {
        let meta_path = format!("{}.scripts", self.db_path);
        if let Ok(data) = fs::read_to_string(&meta_path) {
            let metas: Vec<ScriptMeta> = serde_json::from_str(&data)?;
            for meta in metas {
                // self.eval_register(&meta.source, Some(&meta.name), meta.desc.as_deref())?;
            }
        }
        Ok(())
    }

    pub fn save_sec_index(&self) -> Result<()> {
        let path = format!("{}.secindex", self.db_path);
        let data = serde_json::to_vec(&self.sec_index)?;
        std::fs::write(path, data)?;
        Ok(())
    }

    pub fn load_sec_index(&mut self) -> Result<()> {
        let path = format!("{}.secindex", self.db_path);
        if let Ok(data) = std::fs::read(&path) {
            self.sec_index = serde_json::from_slice(&data)?;
        }
        Ok(())
    }

    /// Set a field inside a JSON object (at key). Creates object if needed.
    /// value may be raw JSON or string.
    pub fn json_set_field(&mut self, key: &str, field: &str, value: &str) -> Result<()> {
        let old_val = self.get(key);
        let mut root: serde_json::Value = old_val
            .as_ref()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_else(|| serde_json::json!({}));

        // Try to parse value as JSON, else treat as string
        let new_val = serde_json
            ::from_str(value)
            .unwrap_or(serde_json::Value::String(value.to_string()));

        if let serde_json::Value::Object(ref mut map) = root {
            map.insert(field.to_string(), new_val);
            let new_json = serde_json::to_string(&root)?;
            // --- update index on JSON field set ---
            self.sec_index.update(key, old_val.as_deref(), Some(&new_json));
            self.save_sec_index().ok(); // <-- persist index after update
            self.put(key, &new_json)
        } else {
            // If not an object, overwrite with a new object
            let mut map = serde_json::Map::new();
            map.insert(field.to_string(), new_val);
            let root = serde_json::Value::Object(map);
            let new_json = serde_json::to_string(&root)?;
            self.sec_index.update(key, old_val.as_deref(), Some(&new_json));
            self.save_sec_index().ok(); // <-- persist index after update
            self.put(key, &new_json)
        }
    }

    /// Get a field from a JSON object (at key). Returns value as string (raw JSON).
    pub fn json_get_field(&mut self, key: &str, field: &str) -> Option<String> {
        self.get(key)
            .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
            .and_then(|v| v.get(field).map(|v| v.to_string()))
    }

    /// Push a value onto a JSON array at key.
    pub fn list_push(&mut self, key: &str, value: &str) -> Result<()> {
        let mut arr = self
            .get(key)
            .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
            .unwrap_or(serde_json::Value::Array(vec![]));

        if let serde_json::Value::Array(ref mut vec) = arr {
            vec.push(serde_json::Value::String(value.to_string()));
        } else {
            // Overwrite if not array
            arr = serde_json::Value::Array(vec![serde_json::Value::String(value.to_string())]);
        }
        let new_json = serde_json::to_string(&arr)?;
        self.put(key, &new_json)
    }

    /// Add a unique value to a JSON set (array) at key.
    pub fn set_add(&mut self, key: &str, value: &str) -> Result<()> {
        let mut arr = self
            .get(key)
            .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
            .unwrap_or(serde_json::Value::Array(vec![]));

        if let serde_json::Value::Array(ref mut vec) = arr {
            if !vec.iter().any(|v| v == &serde_json::Value::String(value.to_string())) {
                vec.push(serde_json::Value::String(value.to_string()));
            }
        } else {
            // Overwrite if not array
            arr = serde_json::Value::Array(vec![serde_json::Value::String(value.to_string())]);
        }
        let new_json = serde_json::to_string(&arr)?;
        self.put(key, &new_json)
    }

    /// Saves a crash-safe snapshot (fsyncs after copy).
    pub fn snapshot(&mut self, snapshot_path: &str) -> Result<()> {
        self.flush_buffer()?;
        storage::save_hint(&self.db_path, &self.index)?;
        fs::copy(&self.db_path, snapshot_path).map_err(Error::Io)?;
        fsync_file(snapshot_path)?;
        let wal_src = format!("{}.wal", &self.db_path);
        let hint_src = format!("{}.hint", &self.db_path);
        if fs::metadata(&wal_src).is_ok() {
            let wal_dst = format!("{}.wal", snapshot_path);
            fs::copy(&wal_src, &wal_dst).ok();
            fsync_file(&wal_dst).ok();
        }
        if fs::metadata(&hint_src).is_ok() {
            let hint_dst = format!("{}.hint", snapshot_path);
            fs::copy(&hint_src, &hint_dst).ok();
            fsync_file(&hint_dst).ok();
        }
        Ok(())
    }

    /// Restores from a snapshot.
    pub fn restore(&mut self, snapshot_path: &str) -> Result<()> {
        fs::copy(snapshot_path, &self.db_path).map_err(Error::Io)?;
        let wal_src = format!("{}.wal", snapshot_path);
        let hint_src = format!("{}.hint", snapshot_path);
        if fs::metadata(&wal_src).is_ok() {
            fs::copy(&wal_src, &format!("{}.wal", &self.db_path)).ok();
        }
        if fs::metadata(&hint_src).is_ok() {
            fs::copy(&hint_src, &format!("{}.hint", &self.db_path)).ok();
        }
        *self = SlackbaseEngine::open(&self.db_path, self.serializer.box_clone())?;
        Ok(())
    }

    /// Scans keys by prefix or range.
    pub fn scan(
        &mut self,
        prefix: Option<&str>,
        range: Option<(&str, &str)>
    ) -> Vec<(String, Option<String>)> {
        let mut result = Vec::new();
        let mut keys: Vec<String> = self.index.keys().cloned().collect();
        keys.sort();
        for key in keys {
            if let Some(pfx) = prefix {
                if !key.starts_with(pfx) {
                    continue;
                }
            }
            if let Some((s, e)) = range {
                if key.as_str() < s || key.as_str() > e {
                    continue;
                }
            }
            let value = self.get(&key);
            result.push((key, value));
        }
        result
    }

    /// Returns human-readable statistics.
    pub fn stats(&self) -> String {
        let db_size = fs
            ::metadata(&self.db_path)
            .map(|m| m.len())
            .unwrap_or(0);
        let wal_size = fs
            ::metadata(format!("{}.wal", &self.db_path))
            .map(|m| m.len())
            .unwrap_or(0);
        let hint_size = fs
            ::metadata(format!("{}.hint", &self.db_path))
            .map(|m| m.len())
            .unwrap_or(0);
        let total = db_size + wal_size + hint_size;
        format!(
            "Reads: {}\nWrites: {}\nHits: {}\nMisses: {}\n\
            Total keys: {}\nDB size: {} bytes\nWAL size: {} bytes\nHint size: {} bytes\nTotal disk usage: {} bytes",
            self.read_ops,
            self.write_ops,
            self.hits,
            self.misses,
            self.index.len(),
            db_size,
            wal_size,
            hint_size,
            total
        )
    }

    pub fn hash_set(&mut self, key: &str, field: &str, value: &str) -> Result<()> {
        let mut obj = self
            .get(key)
            .and_then(|s|
                serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(&s).ok()
            )
            .unwrap_or_default();
        obj.insert(field.to_string(), serde_json::Value::String(value.to_string()));
        let new_json = serde_json::to_string(&obj)?;
        self.put(key, &new_json)
    }

    pub fn hash_get(&mut self, key: &str, field: &str) -> Option<String> {
        self.get(key)
            .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
            .and_then(|v| v.get(field).map(|v| v.to_string()))
    }

    pub fn hash_del(&mut self, key: &str, field: &str) -> Result<()> {
        let mut obj = self
            .get(key)
            .and_then(|s|
                serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(&s).ok()
            )
            .unwrap_or_default();
        obj.remove(field);
        let new_json = serde_json::to_string(&obj)?;
        self.put(key, &new_json)
    }

    pub fn hash_getall(&mut self, key: &str) -> Option<HashMap<String, String>> {
        self.get(key)
            .and_then(|s|
                serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(&s).ok()
            )
            .map(|map|
                map
                    .into_iter()
                    .map(|(k, v)| (k, v.to_string()))
                    .collect()
            )
    }

    // Push value to the left (head) of the list
    pub fn list_lpush(&mut self, key: &str, value: &str) -> Result<()> {
        let mut arr = self
            .get(key)
            .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
            .unwrap_or(serde_json::Value::Array(vec![]));

        if let serde_json::Value::Array(ref mut vec) = arr {
            vec.insert(0, serde_json::Value::String(value.to_string()));
        } else {
            arr = serde_json::Value::Array(vec![serde_json::Value::String(value.to_string())]);
        }
        let new_json = serde_json::to_string(&arr)?;
        self.put(key, &new_json)
    }

    // Push value to the right (tail) of the list
    pub fn list_rpush(&mut self, key: &str, value: &str) -> Result<()> {
        let mut arr = self
            .get(key)
            .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
            .unwrap_or(serde_json::Value::Array(vec![]));

        if let serde_json::Value::Array(ref mut vec) = arr {
            vec.push(serde_json::Value::String(value.to_string()));
        } else {
            arr = serde_json::Value::Array(vec![serde_json::Value::String(value.to_string())]);
        }
        let new_json = serde_json::to_string(&arr)?;
        self.put(key, &new_json)
    }

    // Pop value from the left (head) of the list
    pub fn list_lpop(&mut self, key: &str) -> Option<String> {
        let mut arr = self
            .get(key)
            .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())?;

        if let serde_json::Value::Array(ref mut vec) = arr {
            if !vec.is_empty() {
                let val = vec.remove(0).to_string();
                let new_json = serde_json::to_string(&arr).ok()?;
                let _ = self.put(key, &new_json);
                return Some(val);
            }
        }
        None
    }

    // Pop value from the right (tail) of the list
    pub fn list_rpop(&mut self, key: &str) -> Option<String> {
        let mut arr = self
            .get(key)
            .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())?;

        if let serde_json::Value::Array(ref mut vec) = arr {
            if !vec.is_empty() {
                let val = vec.pop().unwrap().to_string();
                let new_json = serde_json::to_string(&arr).ok()?;
                let _ = self.put(key, &new_json);
                return Some(val);
            }
        }
        None
    }

    // Get a range from the list (like lrange in Redis)
    pub fn list_range(&mut self, key: &str, start: isize, end: isize) -> Option<Vec<String>> {
        let arr = self.get(key).and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())?;

        if let serde_json::Value::Array(vec) = arr {
            let len = vec.len() as isize;
            let s = if start < 0 { len + start } else { start };
            let e = if end < 0 { len + end } else { end };
            let s = s.max(0).min(len);
            let e = e.max(0).min(len - 1);

            if s > e || len == 0 {
                return Some(vec![]);
            }
            Some(
                vec[s as usize..=e as usize]
                    .iter()
                    .map(|v| v.to_string())
                    .collect()
            )
        } else {
            None
        }
    }

    // Get list length
    pub fn list_len(&mut self, key: &str) -> usize {
        self.get(key)
            .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
            .and_then(|v| v.as_array().map(|arr| arr.len()))
            .unwrap_or(0)
    }

    /// Registers and compiles a Lua script, storing metadata.
    pub fn eval_register(
        &mut self,
        src: &str,
        name: Option<&str>,
        desc: Option<&str>
    ) -> Result<String> {
        let mut hasher = Sha1::new();
        hasher.update(src.as_bytes());
        let sha = hex::encode(hasher.finalize());

        if !self.scripts.contains_key(&sha) {
            let func = self.lua
                .load(src)
                .into_function()
                .map_err(|_| Error::InvalidRecord)?;
            let func_static: Function<'static> = unsafe { std::mem::transmute(func) };
            self.scripts.insert(sha.clone(), func_static);
        }

        let meta = ScriptMeta {
            name: name.map(|s| s.to_string()).unwrap_or_else(|| sha.clone()),
            sha1: sha.clone(),
            desc: desc.map(|s| s.to_string()),
        };
        self.script_meta.insert(sha.clone(), meta.clone());

        if let Some(n) = name {
            self.script_names.insert(n.to_string(), sha.clone());
        }

        // Save scripts metadata after any change
        self.save_scripts_to_disk().ok();

        Ok(sha)
    }

    /// Executes a script by name or SHA.
    pub fn eval_by_name_or_sha(
        &mut self,
        name_or_sha: &str,
        keys: &[&str],
        args: &[&str]
    ) -> Result<Value> {
        let sha = if self.scripts.contains_key(name_or_sha) {
            name_or_sha.to_string()
        } else if let Some(sha) = self.script_names.get(name_or_sha) {
            sha.clone()
        } else {
            return Err(Error::NotFound);
        };
        self.eval_sha(&sha, keys, args)
    }

    /// Executes a script by SHA.
    pub fn eval_sha(&mut self, sha: &str, keys: &[&str], args: &[&str]) -> Result<Value> {
        use mlua::Error as LuaError;

        let func = match self.scripts.get(sha) {
            Some(f) => f.clone(),
            None => {
                return Err(Error::NotFound);
            }
        };

        // Build DB snapshot
        let db_snapshot = {
            let all_keys: Vec<String> = self.index.keys().cloned().collect();
            let mut pairs = Vec::new();
            for key in all_keys {
                if let Some(val) = self.get(&key) {
                    pairs.push((key, val));
                }
            }
            pairs
        };

        let engine_ptr = self as *mut SlackbaseEngine;
        let globals = self.lua.globals();

        let get_fn = self.lua
            .create_function_mut(move |_, key: String| {
                unsafe { Ok((*engine_ptr).get(&key).unwrap_or_default()) }
            })
            .map_err(|_| Error::InvalidRecord)?;
        globals.set("GET", get_fn).map_err(|_| Error::InvalidRecord)?;

        let set_fn = self.lua
            .create_function_mut(move |_, (key, val): (String, String)| {
                unsafe {
                    (*engine_ptr)
                        .put(&key, &val)
                        .map_err(|_| LuaError::RuntimeError("Failed SET".into()))?;
                }
                Ok(())
            })
            .map_err(|_| Error::InvalidRecord)?;
        globals.set("SET", set_fn).map_err(|_| Error::InvalidRecord)?;

        let del_fn = self.lua
            .create_function_mut(move |_, key: String| {
                unsafe {
                    (*engine_ptr)
                        .delete(&key)
                        .map_err(|_| LuaError::RuntimeError("Failed DEL".into()))?;
                }
                Ok(())
            })
            .map_err(|_| Error::InvalidRecord)?;
        globals.set("DEL", del_fn).map_err(|_| Error::InvalidRecord)?;

        let lua_keys = self.lua.create_table().map_err(|_| Error::InvalidRecord)?;
        for (i, &k) in keys.iter().enumerate() {
            lua_keys.set(i + 1, k).map_err(|_| Error::InvalidRecord)?;
        }
        globals.set("KEYS", lua_keys).map_err(|_| Error::InvalidRecord)?;

        let lua_args = self.lua.create_table().map_err(|_| Error::InvalidRecord)?;
        for (i, &a) in args.iter().enumerate() {
            lua_args.set(i + 1, a).map_err(|_| Error::InvalidRecord)?;
        }
        globals.set("ARGV", lua_args).map_err(|_| Error::InvalidRecord)?;

        // Now create DB table from the snapshot.
        let db_table = self.lua.create_table().map_err(|_| Error::InvalidRecord)?;
        for (key, val) in db_snapshot {
            db_table.set(key, val).map_err(|_| Error::InvalidRecord)?;
        }
        globals.set("DB", db_table).map_err(|_| Error::InvalidRecord)?;

        let res = func.call(()).map_err(|_| Error::InvalidRecord)?;
        Ok(res)
    }

    /// Lists registered script SHAs.
    pub fn list_scripts(&self) -> Vec<String> {
        self.scripts.keys().cloned().collect()
    }
}

/// fsyncs a file at the given path.
fn fsync_file(path: &str) -> Result<()> {
    let file = OpenOptions::new().read(true).write(true).open(path)?;
    file.sync_all()?;
    Ok(())
}

impl Drop for SlackbaseEngine {
    /// Flushes buffer on drop.
    fn drop(&mut self) {
        let _ = self.flush_buffer();
    }
}
