// src/script.rs

use crate::engine::kv::SlackbaseEngine;
use crate::types::{ Error, ScriptMeta };
use mlua;
use std::fs::File;
use std::io::{ self, Read };

pub struct ScriptManager<'a> {
    engine: &'a mut SlackbaseEngine,
}

impl<'a> ScriptManager<'a> {
    pub fn new(engine: &'a mut SlackbaseEngine) -> Self {
        Self { engine }
    }

    pub fn load_script_from_file(
        &mut self,
        filename: &str,
        name: &str,
        desc: Option<&str>
    ) -> Result<String, Error> {
        let mut path = std::path::PathBuf::from("lua_scripts");
        path.push(filename);
        let mut file = File::open(&path).map_err(|_| Error::NotFound)?;
        let mut src = String::new();
        file.read_to_string(&mut src)?; // This will auto-convert via From<std::io::Error>
        self.engine.eval_register(&src, Some(name), desc)
    }

    pub fn begin_script_interactive(
        &mut self,
        name: &str,
        desc: Option<&str>
    ) -> Result<String, Error> {
        println!("Enter Lua script. End with a line containing only END:");
        let mut src = String::new();
        loop {
            let mut line = String::new();
            io::stdin().read_line(&mut line)?;
            if line.trim() == "END" {
                break;
            }
            src.push_str(&line);
        }
        self.engine.eval_register(&src, Some(name), desc)
    }

    pub fn list_scripts(&self) -> Vec<ScriptMeta> {
        self.engine.script_meta.values().cloned().collect()
    }

    pub fn run_script(
        &mut self,
        sha_or_name: &str,
        keys: &[String],
        args: &[String]
    ) -> Result<mlua::Value, Error> {
        let key_refs: Vec<&str> = keys
            .iter()
            .map(|s| s.as_str())
            .collect();
        let arg_refs: Vec<&str> = args
            .iter()
            .map(|s| s.as_str())
            .collect();
        self.engine.eval_by_name_or_sha(sha_or_name, &key_refs, &arg_refs)
    }

    pub fn rename_script(&mut self, old_name: &str, new_name: &str) -> Result<(), Error> {
        if let Some(sha) = self.engine.script_names.remove(old_name) {
            self.engine.script_names.insert(new_name.to_string(), sha.clone());
            if let Some(meta) = self.engine.script_meta.get_mut(&sha) {
                meta.name = new_name.to_string();
            }
            Ok(())
        } else {
            Err(Error::NotFound)
        }
    }

    pub fn remove_script(&mut self, sha_or_name: &str) -> Result<(), Error> {
        // Try name
        let sha = if self.engine.script_names.contains_key(sha_or_name) {
            self.engine.script_names.remove(sha_or_name).unwrap()
        } else if self.engine.script_meta.contains_key(sha_or_name) {
            sha_or_name.to_string()
        } else {
            return Err(Error::NotFound);
        };
        self.engine.scripts.remove(&sha);
        self.engine.script_meta.remove(&sha);
        // Optionally: remove script file, update disk, etc.
        Ok(())
    }
}
