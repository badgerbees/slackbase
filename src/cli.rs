use crate::engine::kv::SlackbaseEngine;
use crate::serialization::plain::PlainSerializer;
use crate::serialization::json::JsonSerializer;
use crate::serialization::Serializer;
use crate::engine::batch::BatchOp;

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
                match engine.eval_register(&src) {
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
                    Ok(val) => {
                        match val {
                            mlua::Value::Table(t) => {
                                let mut table = Table::new();
                                table.add_row(Row::new(vec![Cell::new("Idx"), Cell::new("Value")]));
                                let mut idx = 1;
                                loop {
                                    match t.get::<_, mlua::Value>(idx) {
                                        Ok(mlua::Value::Nil) | Err(_) => {
                                            break;
                                        }
                                        Ok(v) => {
                                            let s = match v {
                                                mlua::Value::String(s) =>
                                                    s.to_str().unwrap_or("").to_string(),
                                                mlua::Value::Number(n) => n.to_string(),
                                                mlua::Value::Boolean(b) => b.to_string(),
                                                mlua::Value::Table(_) => "[table]".to_string(),
                                                mlua::Value::Nil => "null".to_string(),
                                                _ => "unsupported".to_string(),
                                            };
                                            table.add_row(
                                                Row::new(
                                                    vec![Cell::new(&idx.to_string()), Cell::new(&s)]
                                                )
                                            );
                                        }
                                    }
                                    idx += 1;
                                }
                                table.printstd();
                            }
                            v => {
                                // Print single value nicely
                                let pretty = match v {
                                    mlua::Value::String(s) => s.to_str().unwrap_or("").to_string(),
                                    mlua::Value::Number(n) => n.to_string(),
                                    mlua::Value::Boolean(b) => b.to_string(),
                                    mlua::Value::Nil => "null".to_string(),
                                    _ => format!("{:?}", v),
                                };
                                println!("Result: {}", pretty);
                            }
                        }
                    }
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
            ["script", "load", filename] => {
                // Always search inside ./lua_scripts/
                let mut path = std::path::PathBuf::from("lua_scripts");
                path.push(filename);

                let mut file = std::fs::File
                    ::open(&path)
                    .unwrap_or_else(|_| panic!("Cannot open script file: {}", path.display()));
                let mut src = String::new();
                use std::io::Read;
                file.read_to_string(&mut src).expect("Cannot read script file");
                let mut engine = db.lock().unwrap();
                match engine.eval_register(&src) {
                    Ok(sha) => println!("Script cached, SHA1={}", sha),
                    Err(e) => println!("Error compiling script: {:?}", e),
                }
            }

            ["script", "begin"] => {
                println!("Enter Lua script. End with a line containing only END:");
                let mut src = String::new();
                loop {
                    let mut line = String::new();
                    std::io::stdin().read_line(&mut line).unwrap();
                    if line.trim() == "END" {
                        break;
                    }
                    src.push_str(&line);
                }
                let mut engine = db.lock().unwrap();
                match engine.eval_register(&src) {
                    Ok(sha) => println!("Script cached, SHA1={}", sha),
                    Err(e) => println!("Error compiling script: {:?}", e),
                }
            }
            ["script", "list"] => {
                let engine = db.lock().unwrap();
                let list = engine.list_scripts();
                for sha in list {
                    println!("{}", sha);
                }
            }
            ["script", "run", sha, tail @ ..] => {
                // Syntax: script run <sha> key1 key2 ... -- arg1 arg2 ...
                let mut split = tail.split(|&s| s == "--");
                let keys = split.next().unwrap_or(&[]).to_vec();
                let args = split.next().unwrap_or(&[]).to_vec();
                let mut engine = db.lock().unwrap();
                match engine.eval_sha(sha, &keys, &args) {
                    Ok(val) =>
                        match val {
                            mlua::Value::Table(t) => {
                                let mut vec = Vec::new();
                                let mut idx = 1;
                                loop {
                                    match t.get::<_, mlua::Value>(idx) {
                                        Ok(mlua::Value::Nil) | Err(_) => {
                                            break;
                                        }
                                        Ok(v) => vec.push(format!("{:?}", v)),
                                    }
                                    idx += 1;
                                }
                                println!("Result: [{}]", vec.join(", "));
                            }
                            v => println!("Result: {:?}", v),
                        }
                    Err(e) => println!("Error running script: {:?}", e),
                }
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
