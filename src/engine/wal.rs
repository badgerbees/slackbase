use std::fs::{OpenOptions, File};
use std::io::{Write, BufWriter, BufRead, BufReader};

pub struct WAL {
    writer: BufWriter<File>,
    path: String,
}

impl WAL {
    pub fn open(path: &str) -> std::io::Result<Self> {
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)?;
        Ok(Self {
            writer: BufWriter::new(file),
            path: path.to_string(),
        })
    }

    /// Append a record (command) to WAL.
    pub fn append(&mut self, record: &str) -> std::io::Result<()> {
        writeln!(self.writer, "{}", record)
    }

    /// Explicitly flush WAL to disk (call this after a batch, or after END).
    pub fn flush(&mut self) -> std::io::Result<()> {
        self.writer.flush()
    }

    /// Re-read all records for recovery.
    pub fn iter(&self) -> std::io::Result<Vec<String>> {
        let file = File::open(&self.path)?;
        let reader = BufReader::new(file);
        Ok(reader.lines().filter_map(Result::ok).collect())
    }

    /// Truncate WAL (after compaction/checkpoint).
    pub fn clear(&mut self) -> std::io::Result<()> {
        let file = OpenOptions::new()
            .write(true)
            .truncate(true)
            .open(&self.path)?;
        file.set_len(0)?;
        Ok(())
    }
}
