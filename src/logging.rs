use mlua::{Value as LuaValue, Table};
use serde_json::{json, Value as JsonValue};

/// Pretty-print any Lua value as human-friendly JSON or fallback string.
pub fn print_lua_value(val: &LuaValue) {
    if let Some(json) = lua_value_to_json(val) {
        println!("{}", serde_json::to_string_pretty(&json).unwrap_or_else(|_| "<invalid-json>".to_string()));
    } else {
        // fallback: print as normal if not table or complex type
        let pretty = match val {
            LuaValue::String(s) => s.to_str().unwrap_or("").to_string(),
            LuaValue::Number(n) => n.to_string(),
            LuaValue::Boolean(b) => b.to_string(),
            LuaValue::Nil => "null".to_string(),
            _ => format!("{:?}", val),
        };
        println!("Result: {}", pretty);
    }
}

/// Convert LuaValue (recursively) into serde_json::Value if possible.
/// Supports tables (both array-like and map-like), strings, numbers, bool, nil.
/// Returns None if value is not representable as JSON.
pub fn lua_value_to_json(val: &LuaValue) -> Option<JsonValue> {
    match val {
        LuaValue::Nil => Some(JsonValue::Null),
        LuaValue::Boolean(b) => Some(JsonValue::Bool(*b)),
        LuaValue::Number(n) => Some(json!(n)),
        LuaValue::String(s) => Some(JsonValue::String(s.to_str().unwrap_or("").to_string())),
        LuaValue::Table(t) => table_to_json(t),
        // Ignore other types (functions, userdata, thread, lightuserdata)
        _ => None,
    }
}

/// Converts a Lua Table into serde_json::Value (Object or Array)
fn table_to_json(table: &Table) -> Option<JsonValue> {
    // Try as array: check if all keys are integer indices from 1..N with no gaps
    let mut max_idx = 0;
    let mut min_idx = usize::MAX;
    let mut count = 0;
    let mut is_array = true;
    let mut array_elems = Vec::new();

    for pair in table.clone().pairs::<LuaValue, LuaValue>() {
        if let Ok((key, value)) = pair {
            match key {
                LuaValue::Integer(i) if i > 0 => {
                    let idx = i as usize;
                    if idx < min_idx { min_idx = idx; }
                    if idx > max_idx { max_idx = idx; }
                    count += 1;
                    array_elems.push((idx, value));
                }
                _ => {
                    is_array = false;
                    break;
                }
            }
        }
    }

    if is_array && count > 0 && min_idx == 1 && max_idx == count && array_elems.len() == count {
        // Sorted array values
        array_elems.sort_by_key(|(idx, _)| *idx);
        let arr = array_elems
            .into_iter()
            .map(|(_, v)| lua_value_to_json(&v).unwrap_or(JsonValue::Null))
            .collect::<Vec<_>>();
        return Some(JsonValue::Array(arr));
    }

    // Otherwise, treat as map/object
    let mut map = serde_json::Map::new();
    for pair in table.clone().pairs::<LuaValue, LuaValue>() {
        if let Ok((key, value)) = pair {
            let kstr = match &key {
                LuaValue::String(s) => s.to_str().unwrap_or("").to_string(),
                LuaValue::Number(n) => n.to_string(),
                LuaValue::Integer(i) => i.to_string(),
                _ => continue, // skip keys that can't be stringified
            };
            let vjson = lua_value_to_json(&value).unwrap_or(JsonValue::Null);
            map.insert(kstr, vjson);
        }
    }
    Some(JsonValue::Object(map))
}
