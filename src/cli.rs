use crate::engine::kv::SlackbaseEngine;
use crate::serialization::plain::PlainSerializer;
use crate::serialization::json::JsonSerializer;
use crate::serialization::Serializer;
use crate::engine::batch::BatchOp;
use crate::script::ScriptManager;
use crate::logging::print_lua_value;

use std::io::{ self, Write };
use std::sync::{ Arc, Mutex };
use crate::types::Error;

use prettytable::{ Table, Row, Cell };

pub fn run() {
    let serializer: Box<dyn Serializer>;

    loop {
        println!("Choose serialization format [plain/json]:");
        print!("> ");
        io::stdout().flush().unwrap();

        let mut ser_input = String::new();
        if io::stdin().read_line(&mut ser_input).is_err() {
            println!("Failed to read input. Please try again.");
            continue;
        }

        match ser_input.trim().to_lowercase().as_str() {
            "plain" => {
                serializer = Box::new(PlainSerializer);
                break;
            }
            "json" => {
                serializer = Box::new(JsonSerializer);
                break;
            }
            other => {
                println!("Invalid input '{}'. Please enter 'plain' or 'json'.", other);
            }
        }
    }

    // Then continue with opening DB and CLI loop as you had:
    let db = Arc::new(
        Mutex::new(SlackbaseEngine::open("slackbase.db", serializer).expect("Failed to open DB"))
    );

    // CLI loop
    loop {
        print!("slackbase> ");
        io::stdout().flush().unwrap();

        let mut input = String::new();
        if io::stdin().read_line(&mut input).is_err() {
            break;
        }

        let args: Vec<&str> = input.trim().split_whitespace().collect();
        match args.as_slice() {
            ["put", key, value] => {
                let mut engine = db.lock().unwrap();
                engine.put(key, value).unwrap();
                println!("OK");
            }

            ["putex", key, value, ttl] => {
                let ttl_secs: u64 = match ttl.parse() {
                    Ok(n) => n,
                    Err(_) => {
                        println!("Invalid TTL (must be a number of seconds)");
                        continue;
                    }
                };
                let mut engine = db.lock().unwrap();
                engine.putex(key, value, ttl_secs).unwrap();
                println!("OK (expires in {} seconds)", ttl_secs);
            }

            ["get", key] => {
                let mut engine = db.lock().unwrap();
                match engine.get(key) {
                    Some(val) => println!("{}", val),
                    None => println!("(nil)"),
                }
            }

            ["del", key] => {
                let mut engine = db.lock().unwrap();
                engine.delete(key).unwrap();
                println!("OK");
            }

            ["compact"] => {
                let mut engine = db.lock().unwrap();
                engine.compact().unwrap();
                println!("Compaction complete. Old records removed.");
            }

            ["snapshot", filename] => {
                let mut engine = db.lock().unwrap();
                engine.snapshot(filename).unwrap();
                println!("Snapshot saved to {}", filename);
            }

            ["restore", filename] => {
                let mut engine = db.lock().unwrap();
                engine.restore(filename).unwrap();
                println!("Database restored from {}", filename);
            }

            ["find", field, value] => {
                let engine = db.lock().unwrap();
                let keys = engine.sec_index.find(field, value);
                if keys.is_empty() {
                    println!("No keys found with {} = {}", field, value);
                } else {
                    println!("Keys with {} = {}:", field, value);
                    for k in keys {
                        println!("- {}", k);
                    }
                }
            }

            ["batch", tail @ ..] => {
                let mut ops = Vec::new();
                let mut iter = tail.iter();
                while let Some(&cmd) = iter.next() {
                    match cmd {
                        "put" => {
                            let k = iter.next().expect("No key for put");
                            let v = iter.next().expect("No value for put");
                            ops.push(BatchOp::Put(k.to_string(), v.to_string()));
                        }
                        "del" => {
                            let k = iter.next().expect("No key for del");
                            ops.push(BatchOp::Del(k.to_string()));
                        }
                        other => println!("Unknown batch op: {}", other),
                    }
                }
                let mut engine = db.lock().unwrap();
                engine.batch(ops).unwrap();
                println!("Batch OK");
            }

            ["scan"] => {
                let mut engine = db.lock().unwrap();
                for (k, v) in engine.scan(None, None) {
                    match v {
                        Some(val) => println!("{} => {}", k, val),
                        None => println!("{} => (expired or deleted)", k),
                    }
                }
            }

            ["scan", prefix] => {
                let mut engine = db.lock().unwrap();
                for (k, v) in engine.scan(Some(prefix), None) {
                    match v {
                        Some(val) => println!("{} => {}", k, val),
                        None => println!("{} => (expired or deleted)", k),
                    }
                }
            }

            ["scan", start, end] => {
                let mut engine = db.lock().unwrap();
                for (k, v) in engine.scan(None, Some((start, end))) {
                    match v {
                        Some(val) => println!("{} => {}", k, val),
                        None => println!("{} => (expired or deleted)", k),
                    }
                }
            }

            ["stats"] => {
                let engine = db.lock().unwrap();
                println!("{}", engine.stats());
            }

            ["eval", tail @ ..] => {
                let src = tail.join(" ");
                let mut engine = db.lock().unwrap();
                // Add name/desc as needed, or use None for now
                match engine.eval_register(&src, None, None) {
                    Ok(sha) => println!("Script cached, SHA1={}", sha),
                    Err(e) => println!("Error compiling script: {:?}", e),
                }
            }

            ["evalsha", sha, tail @ ..] => {
                // syntax: evalsha <sha> key1 key2 … -- arg1 arg2 …
                let mut split = tail.split(|&s| s == "--");
                let keys = split.next().unwrap_or(&[]).to_vec();
                let args = split.next().unwrap_or(&[]).to_vec();
                let mut engine = db.lock().unwrap();
                match engine.eval_sha(sha, &keys, &args) {
                    Ok(val) => print_lua_value(&val),
                    Err(e) => {
                        use mlua::Error as LuaError;
                        match &e {
                            Error::Lua(lua_err) =>
                                match lua_err {
                                    LuaError::SyntaxError { message, incomplete_input, .. } => {
                                        println!("Lua syntax error: {}{}", message, if
                                            *incomplete_input
                                        {
                                            " (incomplete input)"
                                        } else {
                                            ""
                                        });
                                    }
                                    LuaError::RuntimeError(msg) => {
                                        println!("Lua runtime error: {}", msg);
                                    }
                                    LuaError::MemoryError(_) => {
                                        println!("Lua out of memory!");
                                    }
                                    LuaError::CallbackError { traceback, cause } => {
                                        println!(
                                            "Lua callback error: {}\nTraceback:\n{}",
                                            cause,
                                            traceback
                                        );
                                    }
                                    _ => println!("Other Lua error: {:?}", lua_err),
                                }
                            other => println!("Error: {:?}", other),
                        }
                    }
                }
            }
            ["script", "load", filename, name, desc @ ..] => {
                let script_desc = if desc.is_empty() { None } else { Some(desc.join(" ")) };
                let mut engine = db.lock().unwrap();
                let mut manager = ScriptManager::new(&mut engine);
                match manager.load_script_from_file(filename, name, script_desc.as_deref()) {
                    Ok(sha) => println!("Script '{}' cached, SHA1={}", name, sha),
                    Err(e) => println!("Error compiling script: {:?}", e),
                }
            }

            ["script", "begin", name, desc @ ..] => {
                let script_desc = if desc.is_empty() { None } else { Some(desc.join(" ")) };
                let mut engine = db.lock().unwrap();
                let mut manager = ScriptManager::new(&mut engine);
                match manager.begin_script_interactive(name, script_desc.as_deref()) {
                    Ok(sha) => println!("Script '{}' cached, SHA1={}", name, sha),
                    Err(e) => println!("Error compiling script: {:?}", e),
                }
            }

            ["script", "list"] => {
                let mut engine = db.lock().unwrap();
                let manager = ScriptManager::new(&mut engine);
                let scripts = manager.list_scripts();
                let mut table = Table::new();
                table.add_row(
                    Row::new(vec![Cell::new("SHA1"), Cell::new("Name"), Cell::new("Description")])
                );
                for meta in scripts {
                    table.add_row(
                        Row::new(
                            vec![
                                Cell::new(&meta.sha1),
                                Cell::new(&meta.name),
                                Cell::new(meta.desc.as_deref().unwrap_or(""))
                            ]
                        )
                    );
                }
                table.printstd();
            }

            ["script", "run", sha_or_name, tail @ ..] => {
                let mut split = tail.split(|&s| s == "--");
                let keys: Vec<String> = split
                    .next()
                    .unwrap_or(&[])
                    .iter()
                    .map(|s| s.to_string())
                    .collect();
                let args: Vec<String> = split
                    .next()
                    .unwrap_or(&[])
                    .iter()
                    .map(|s| s.to_string())
                    .collect();
                let mut engine = db.lock().unwrap();
                let mut manager = ScriptManager::new(&mut engine);
                match manager.run_script(sha_or_name, &keys, &args) {
                    Ok(val) => print_lua_value(&val),
                    Err(e) => println!("Error running script: {:?}", e),
                }
            }

            ["script", "rename", old_name, new_name] => {
                let mut engine = db.lock().unwrap();
                let mut manager = ScriptManager::new(&mut engine);
                match manager.rename_script(old_name, new_name) {
                    Ok(()) => println!("Script '{}' renamed to '{}'", old_name, new_name),
                    Err(_) => println!("Script name '{}' not found", old_name),
                }
            }
            ["script", "remove", sha_or_name] => {
                let mut engine = db.lock().unwrap();
                let mut manager = ScriptManager::new(&mut engine);
                match manager.remove_script(sha_or_name) {
                    Ok(()) => println!("Script '{}' removed.", sha_or_name),
                    Err(_) => println!("Script '{}' not found.", sha_or_name),
                }
            }

            // JSON commands

            ["json", "set", key, field, value] => {
                let mut engine = db.lock().unwrap();
                match engine.json_set_field(key, field, value) {
                    Ok(_) => println!("OK"),
                    Err(e) => println!("ERR: {:?}", e),
                }
            }

            ["json", "get", key, field] => {
                let mut engine = db.lock().unwrap();
                match engine.json_get_field(key, field) {
                    Some(val) => println!("{}", val),
                    None => println!("(nil)"),
                }
            }
            ["list", "push", key, value] => {
                let mut engine = db.lock().unwrap();
                match engine.list_push(key, value) {
                    Ok(_) => println!("OK (pushed '{}' to list '{}')", value, key),
                    Err(e) => println!("ERR: {:?}", e),
                }
            }

            ["list", "show", key] | ["set", "show", key] => {
                let mut engine = db.lock().unwrap();
                match engine.get(key) {
                    Some(val) => println!("{}", val),
                    None => println!("(nil)"),
                }
            }

            ["set", "add", key, value] => {
                let mut engine = db.lock().unwrap();
                match engine.set_add(key, value) {
                    Ok(_) => println!("OK (added '{}' to set '{}')", value, key),
                    Err(e) => println!("ERR: {:?}", e),
                }
            }

            // Hash JSON commands
            // Set field in a hash (JSON object)
            ["hash", "set", key, field, value] => {
                let mut engine = db.lock().unwrap();
                match engine.hash_set(key, field, value) {
                    Ok(_) => println!("OK (set '{}:{}')", key, field),
                    Err(e) => println!("ERR: {:?}", e),
                }
            }

            // Get field from a hash
            ["hash", "get", key, field] => {
                let mut engine = db.lock().unwrap();
                match engine.hash_get(key, field) {
                    Some(val) => println!("{}", val),
                    None => println!("(nil)"),
                }
            }

            // Delete field from a hash
            ["hash", "del", key, field] => {
                let mut engine = db.lock().unwrap();
                match engine.hash_del(key, field) {
                    Ok(_) => println!("OK (deleted '{}:{}')", key, field),
                    Err(e) => println!("ERR: {:?}", e),
                }
            }

            // Get all fields/values from a hash
            ["hash", "getall", key] => {
                let mut engine = db.lock().unwrap();
                match engine.hash_getall(key) {
                    Some(map) => {
                        for (k, v) in map {
                            println!("{}: {}", k, v);
                        }
                    }
                    None => println!("(nil)"),
                }
            }

            // List commands
            ["list", "lpush", key, value] => {
                let mut engine = db.lock().unwrap();
                match engine.list_lpush(key, value) {
                    Ok(_) => println!("OK (lpush '{}' to '{}')", value, key),
                    Err(e) => println!("ERR: {:?}", e),
                }
            }
            ["list", "rpush", key, value] => {
                let mut engine = db.lock().unwrap();
                match engine.list_rpush(key, value) {
                    Ok(_) => println!("OK (rpush '{}' to '{}')", value, key),
                    Err(e) => println!("ERR: {:?}", e),
                }
            }
            ["list", "lpop", key] => {
                let mut engine = db.lock().unwrap();
                match engine.list_lpop(key) {
                    Some(val) => println!("{}", val),
                    None => println!("(nil)"),
                }
            }
            ["list", "rpop", key] => {
                let mut engine = db.lock().unwrap();
                match engine.list_rpop(key) {
                    Some(val) => println!("{}", val),
                    None => println!("(nil)"),
                }
            }
            ["list", "range", key, start, end] => {
                let mut engine = db.lock().unwrap();
                let s = start.parse().unwrap_or(0);
                let e = end.parse().unwrap_or(0);
                match engine.list_range(key, s, e) {
                    Some(items) if !items.is_empty() => {
                        for item in items {
                            println!("{}", item);
                        }
                    }
                    _ => println!("(nil)"),
                }
            }
            ["list", "len", key] => {
                let mut engine = db.lock().unwrap();
                let len = engine.list_len(key);
                println!("{}", len);
            }

            ["exit"] | ["quit"] => {
                break;
            }

            _ =>
                println!(
                    "Usage: \
                put <key> <value> | \
                putex <key> <value> <ttl_secs> | \
                get <key> | del <key> | compact | \
                snapshot <file> | restore <file> | \
                batch ... | scan [prefix] | scan <start> <end> | \
                stats | eval <lua_src> | evalsha <sha> [keys] -- [args] | exit"
                ),
        }
    }
}
