use std::fs::{File, OpenOptions};
use std::io::{self, BufRead, BufReader, Seek, SeekFrom, Write};
use std::collections::HashMap;
use memmap2::Mmap;
use std::time::{SystemTime, UNIX_EPOCH};

pub fn append_record(path: &str, record: &str) -> io::Result<(u64, usize)> {
    let mut file = OpenOptions::new().append(true).create(true).open(path)?;
    let offset = file.seek(SeekFrom::End(0))?;
    let line = format!("{}\n", record);
    file.write_all(line.as_bytes())?;
    Ok((offset, line.len()))
}

/// Read all records from file as Vec<(key, full_value_str)>
/// `full_value_str` is the rest after the first tab, e.g. `put\t...` or `del\t...`
pub fn read_records(path: &str) -> io::Result<Vec<(String, String)>> {
    let file = match File::open(path) {
        Ok(f) => f,
        Err(ref e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };
    let reader = BufReader::new(file);
    let mut records = Vec::new();

    for line in reader.lines() {
        if let Ok(l) = line {
            let parts: Vec<_> = l.splitn(2, '\t').collect();
            if parts.len() == 2 {
                records.push((parts[0].to_string(), parts[1].to_string()));
            }
        }
    }
    Ok(records)
}

/// Read a slice from file at offset and length, return as String
pub fn read_record_slice(path: &str, offset: u64, len: usize) -> io::Result<Option<String>> {
    let file = File::open(path)?;
    let mmap = unsafe { Mmap::map(&file)? };
    let end = (offset as usize).saturating_add(len);
    if end > mmap.len() {
        return Ok(None);
    }
    let slice = &mmap[offset as usize..end];
    let line = std::str::from_utf8(slice)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?
        .trim_end();
    Ok(Some(line.to_string()))
}

/// Compact the log file by keeping only the latest valid (not deleted, not expired) record per key
pub fn compact_log(path: &str) -> io::Result<()> {
    let records = read_records(path)?;
    let mut latest: HashMap<String, (String, Option<u64>)> = HashMap::new(); // key -> (base64_value, expiry)

    let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs();

    for (key, value) in records {
        if value.is_empty() {
            // Deleted record
            latest.remove(&key);
            continue;
        }

        // Value format expected: "put\tbase64_value\texpiry?"
        let parts: Vec<&str> = value.split('\t').collect();

        if parts[0] == "put" {
            let base64_val = parts.get(1).unwrap_or(&"").to_string();
            let expiry = parts.get(2).and_then(|s| s.parse::<u64>().ok());

            // If expired, remove key if exists
            if let Some(expiry_ts) = expiry {
                if now > expiry_ts {
                    latest.remove(&key);
                    continue;
                }
            }

            latest.insert(key, (base64_val, expiry));
        } else if parts[0] == "del" {
            latest.remove(&key);
        }
    }

    // Rewrite file atomically
    let tmp_path = format!("{}.compact", path);
    let mut file = File::create(&tmp_path)?;

    for (key, (base64_val, expiry)) in latest {
        if let Some(exp) = expiry {
            writeln!(file, "{}\tput\t{}\t{}", key, base64_val, exp)?;
        } else {
            writeln!(file, "{}\tput\t{}\t", key, base64_val)?;
        }
    }

    std::fs::rename(tmp_path, path)?;
    Ok(())
}

/// Build an offset index for the latest valid records only
/// Returns map: key -> (offset, length)
pub fn build_offset_index(path: &str) -> io::Result<HashMap<String, (u64, usize)>> {
    let file = match File::open(path) {
        Ok(f) => f,
        Err(ref e) if e.kind() == io::ErrorKind::NotFound => return Ok(HashMap::new()),
        Err(e) => return Err(e),
    };

    let mmap = unsafe { Mmap::map(&file)? };
    let mut idx = HashMap::new();
    let mut offset = 0u64;

    let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs();

    for line in mmap.split(|b| *b == b'\n') {
        if line.is_empty() {
            continue;
        }
        if let Some(tab) = line.iter().position(|b| *b == b'\t') {
            let key = String::from_utf8_lossy(&line[..tab]).to_string();
            let rest = &line[tab + 1..];
            let rest_str = String::from_utf8_lossy(rest).to_string();

            if rest_str.is_empty() {
                idx.remove(&key);
            } else {
                // rest_str like: put\tbase64_value\texpiry?
                let parts: Vec<&str> = rest_str.split('\t').collect();
                if parts[0] == "put" {
                    let expiry = parts.get(2).and_then(|s| s.parse::<u64>().ok());
                    if let Some(exp) = expiry {
                        if now > exp {
                            idx.remove(&key);
                            offset += (line.len() as u64) + 1;
                            continue;
                        }
                    }
                    idx.insert(key, (offset, line.len()));
                } else if parts[0] == "del" {
                    idx.remove(&key);
                }
            }
        }
        offset += (line.len() as u64) + 1;
    }

    Ok(idx)
}

/// Save index hint file in CSV format: key,offset,length
pub fn save_hint(path: &str, index: &HashMap<String, (u64, usize)>) -> io::Result<()> {
    let hint_path = format!("{}.hint", path);
    let mut file = File::create(&hint_path)?;

    for (k, (off, len)) in index {
        writeln!(file, "{},{},{}", k, off, len)?;
    }
    Ok(())
}

/// Load index hint file from CSV
pub fn load_hint(path: &str) -> io::Result<HashMap<String, (u64, usize)>> {
    let hint_path = format!("{}.hint", path);
    let file = File::open(&hint_path)?;
    let reader = BufReader::new(file);
    let mut map = HashMap::new();

    for line in reader.lines() {
        let l = line?;
        let parts: Vec<_> = l.split(',').collect();
        if parts.len() == 3 {
            let key = parts[0].to_string();
            if let (Ok(offset), Ok(len)) = (parts[1].parse(), parts[2].parse()) {
                map.insert(key, (offset, len));
            }
        }
    }
    Ok(map)
}
