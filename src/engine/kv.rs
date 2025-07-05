use crate::storage::file as storage;
use crate::engine::wal::WAL;
use crate::types::{ Result, Error }; // <--- Import your own Error type
use std::collections::HashMap;
use std::fs;
use std::io::{ self, ErrorKind };
use crate::serialization::Serializer;
use base64::{ engine::general_purpose, Engine };
use crate::engine::batch::BatchOp;
use std::time::{ SystemTime, UNIX_EPOCH };

// For Lua scripting support
use mlua::{ Lua, Function, Value };
use sha1::{ Sha1, Digest };
use hex;

const BATCH_SIZE: usize = 10;

pub struct SlackbaseEngine {
    db_path: String,
    index: HashMap<String, (u64, usize)>,
    wal: WAL,
    write_buffer: Vec<String>,
    serializer: Box<dyn Serializer>,

    // Metrics
    pub read_ops: usize,
    pub write_ops: usize,
    pub hits: usize,
    pub misses: usize,

    // Lua scripting engine
    lua: Lua,
    scripts: HashMap<String, Function<'static>>,
}

impl SlackbaseEngine {
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

        Ok(Self {
            db_path: db_path.to_string(),
            index,
            wal,
            write_buffer: Vec::new(),
            serializer,
            read_ops: 0,
            write_ops: 0,
            hits: 0,
            misses: 0,
            lua,
            scripts,
        })
    }

    fn flush_buffer(&mut self) -> Result<()> {
        for record in &self.write_buffer {
            self.wal.append(record)?; // Use your error type here
        }
        self.write_buffer.clear();
        Ok(())
    }

    fn put_internal(&mut self, key: &str, value: &str, expires_at: Option<u64>) -> Result<()> {
        self.write_ops += 1;
        let encoded = self.serializer.serialize(value)?;
        let encoded_str = general_purpose::STANDARD.encode(&encoded);
        let record = match expires_at {
            Some(ts) => format!("put\t{}\t{}\t{}", key, encoded_str, ts),
            None => format!("put\t{}\t{}\t", key, encoded_str),
        };
        self.write_buffer.push(record.clone());
        self.flush_buffer()?;
        let (offset, len) = storage::append_record(&self.db_path, &record)?;
        self.index.insert(key.to_string(), (offset, len));
        storage::save_hint(&self.db_path, &self.index)?;
        Ok(())
    }

    pub fn put(&mut self, key: &str, value: &str) -> Result<()> {
        self.put_internal(key, value, None)
    }

    pub fn putex(&mut self, key: &str, value: &str, ttl_secs: u64) -> Result<()> {
        let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
        self.put_internal(key, value, Some(now + ttl_secs))
    }

    pub fn get(&mut self, key: &str) -> Option<String> {
        self.read_ops += 1;
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

        self.hits += 1;
        Some(value)
    }

    pub fn delete(&mut self, key: &str) -> Result<()> {
        self.write_ops += 1;
        let record = format!("del\t{}", key);
        self.write_buffer.push(record.clone());
        self.flush_buffer()?;
        let (_off, _len) = storage::append_record(&self.db_path, &record)?;
        self.index.remove(key);
        storage::save_hint(&self.db_path, &self.index)?;
        Ok(())
    }

    pub fn compact(&mut self) -> Result<()> {
        self.flush_buffer()?;
        storage::compact_log(&self.db_path)?;

        self.index = storage::build_offset_index(&self.db_path)?;
        storage::save_hint(&self.db_path, &self.index)?;

        self.wal.clear()?;
        *self = SlackbaseEngine::open(&self.db_path, self.serializer.box_clone())?;
        Ok(())
    }

    pub fn batch(&mut self, ops: Vec<BatchOp>) -> Result<()> {
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

    pub fn snapshot(&mut self, snapshot_path: &str) -> Result<()> {
        let mut_self = unsafe { &mut *(self as *const Self as *mut Self) };
        mut_self.flush_buffer()?;
        storage::save_hint(&self.db_path, &self.index)?;
        fs::copy(&self.db_path, snapshot_path).map_err(Error::Io)?;
        let wal_src = format!("{}.wal", &self.db_path);
        let hint_src = format!("{}.hint", &self.db_path);
        if fs::metadata(&wal_src).is_ok() {
            fs::copy(&wal_src, &format!("{}.wal", snapshot_path)).ok();
        }
        if fs::metadata(&hint_src).is_ok() {
            fs::copy(&hint_src, &format!("{}.hint", snapshot_path)).ok();
        }
        Ok(())
    }

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

    /// Compile & cache a Lua script; returns its SHA1
    pub fn eval_register(&mut self, src: &str) -> Result<String> {
        let mut hasher = Sha1::new();
        hasher.update(src.as_bytes());
        let sha = hex::encode(hasher.finalize());

        if !self.scripts.contains_key(&sha) {
            let func = self.lua
                .load(src)
                .into_function()
                .map_err(|e| Error::InvalidRecord)?; // <-- Convert mlua errors to your own error type
            let func_static: Function<'static> = unsafe { std::mem::transmute(func) };
            self.scripts.insert(sha.clone(), func_static);
        }
        Ok(sha)
    }

    pub fn eval_sha(&mut self, sha: &str, keys: &[&str], args: &[&str]) -> Result<Value> {
        use mlua::Error as LuaError;

        let func = match self.scripts.get(sha) {
            Some(f) => f.clone(),
            None => {
                return Err(Error::NotFound);
            }
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

        let res = func.call(()).map_err(|_| Error::InvalidRecord)?;
        Ok(res)
    }

    pub fn list_scripts(&self) -> Vec<String> {
        self.scripts.keys().cloned().collect()
    }
}

impl Drop for SlackbaseEngine {
    fn drop(&mut self) {
        let _ = self.flush_buffer();
    }
}
