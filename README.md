# Slackbase

Slackbase is a key-value store database engine.

Note that I made this project as a way to learn Rust so it's not meant to be serious (hence why I call it "slack"-base) however I'm always open for feedbacks and suggestions.

## Features

*   **Key-Value Storage:** Store and retrieve data using simple key-value pairs.
*   **Persistence:** Data is saved to disk using an append-only log format.
*   **Write-Ahead Log (WAL):** Ensures data durability for write operations.
*   **In-Memory Indexing with Hint Files:** Fast key lookups with optimized startup times.
*   **Lua Scripting:** Execute custom, atomic server-side scripts.
*   **Snapshot and Restore:** Create backups and restore database state.
*   **Compaction:** Reclaim disk space by removing old/deleted data.
*   **Time-To-Live (TTL):** Optional automatic expiration for keys.
*   **CLI Interface:** Interactive command-line tool for database operations.

## Slackbase Architecture

Slackbase is designed as a persistent key-value store with a focus on simplicity and durability. Here's an overview of its core components and processes:

### Core Components

*   **`SlackbaseEngine`:** This is the heart of the database. It manages:
    *   An **in-memory index**: A `HashMap` that stores keys and their corresponding byte offset and length within the data file. This allows for quick lookups.
    *   A **Write-Ahead Log (WAL)**: Ensures that write operations (`PUT`, `DEL`) are durable. Changes are first written to the WAL before being applied to the main data file.
    *   **Value Serialization**: Supports pluggable serializers (e.g., JSON, plain text) for values. Internally, values are base64 encoded before being written to disk.
    *   **Metrics Tracking**: Keeps track of operations like reads, writes, cache hits, and misses.

*   **Data Storage (`.db` file):**
    *   The primary data is stored in a single data file (e.g., `database.db`).
    *   This file uses an **append-only log format**. New data or changes (like deletions) are appended to the end of the file.
    *   Each record is newline-separated. A typical `PUT` record looks like: `key\tput\tbase64_encoded_value\t[expiry_timestamp]`. A `DEL` record is simpler, e.g., `key\tdel` (though for `del` operations, they effectively mark a key as removed in the index and are processed out during compaction).

*   **Index and Hint Files (`.hint` file):**
    *   On startup, `SlackbaseEngine` can build its in-memory index by scanning the entire data file.
    *   To accelerate this process, a **hint file** (e.g., `database.db.hint`) can be generated. This file stores a snapshot of the index (key, offset, length in CSV format).
    *   If a valid and up-to-date hint file exists, the engine loads the index from it, significantly speeding up startup times.

### Write Path

When a `PUT` operation occurs:
1.  The value is serialized (if a serializer is configured) and then base64 encoded.
2.  The operation (e.g., `put key encoded_value expiry_timestamp`) is added to an in-memory write buffer.
3.  The buffer is flushed to the **Write-Ahead Log (`.wal` file)** for durability.
4.  The record is then appended to the main **data file (`.db` file)**.
5.  The **in-memory index** is updated with the new key's offset and length in the data file.
6.  The **hint file (`.hint` file)** is updated with the new index information.

### Read Path
When a `GET` operation occurs:
1.  The engine consults the **in-memory index** for the key's offset and length.
2.  If found, the data is read directly from the **data file (`.db` file)** at that specific location, often using memory mapping (`mmap`) for efficiency.
3.  If an expiry timestamp is present on the record, it's checked against the current time. Expired records are treated as if the key doesn't exist.
4.  The base64 encoded value is decoded and then deserialized (if a serializer is configured) before being returned.

### Compaction
Over time, as data is updated and deleted, the append-only data file can grow and contain stale data. The `COMPACT` operation addresses this:
1.  It reads all records from the current data file.
2.  It builds a new, clean representation of the data in memory, containing only the latest, non-expired, and non-deleted key-value pairs.
3.  This clean data is written to a new temporary data file.
4.  The temporary file then atomically replaces the old data file.
5.  The WAL is cleared, and the engine reinitializes its state (including rebuilding the index from the newly compacted file or its hint file).

### Data Expiration
Keys can be set with a Time-To-Live (TTL). This is implemented by storing an absolute `expiry_timestamp` alongside the record in the data file.
*   During `GET` operations, if a record's `expiry_timestamp` is in the past, it's considered expired and not returned.
*   The `COMPACT` process also purges expired records.

## Lua Scripting Engine

Slackbase supports server-side scripting using Lua. This allows for atomic execution of multiple operations and custom logic directly within the database.

The primary way to interact with Lua scripting is via the Slackbase Command Line Interface (CLI).

### Managing and Running Scripts via CLI

**1. Organizing Lua Script Files:**

It's recommended to place your Lua script files (e.g., `my_atomic_update.lua`) in a directory named `lua_scripts` located in the same directory where you run the `slackbase` executable.

**2. Loading Scripts from Files:**

Use the `script load` command to register a script from a file into the engine:

```bash
slackbase> script load my_atomic_update.lua
Script cached, SHA1=abcdef1234567890abcdef1234567890abcdef12
```

This command reads `./lua_scripts/my_atomic_update.lua`, sends its content to the database engine for compilation and caching, and then prints the SHA1 hash associated with the script. You'll use this SHA1 hash to execute the script.

**3. Interactive Script Input:**

For multi-line scripts directly in the CLI, you can use `script begin`:

```bash
slackbase> script begin
Enter Lua script. End with a line containing only END:
return SET(KEYS[1], ARGV[1])
END
Script cached, SHA1=fedcba0987654321fedcba0987654321fedcba09
```

**4. Direct Source Evaluation (for Engine Primitives):**

The CLI also exposes the engine's direct script registration command `eval` for short, single-line scripts:

```bash
slackbase> eval "return GET(KEYS[1])"
Script cached, SHA1=1234567890abcdef1234567890abcdef12345678
```
This also registers the script and returns its SHA1 hash. The `script load` and `script begin` commands are convenient wrappers around this underlying mechanism.

**5. Listing Registered Scripts:**

To see all scripts currently registered (cached) in the engine, use `script list`:

```bash
slackbase> script list
abcdef1234567890abcdef1234567890abcdef12
fedcba0987654321fedcba0987654321fedcba09
1234567890abcdef1234567890abcdef12345678
```
This will output the SHA1 hashes of all scripts known to the current engine instance.

**6. Running Registered Scripts:**

Once a script is registered, you can execute it using its SHA1 hash with the `script run` command (or its alias `evalsha`):

```bash
slackbase> script run <sha1_hash> [key1 key2 ...] -- [arg1 arg2 ...]
```
For example, to run a script with SHA1 `abcdef...`:
```bash
slackbase> script run abcdef1234567890abcdef1234567890abcdef12 user:1:profile -- name "New Name" age 30
Result: ... (output depends on the script)
```
*   Replace `<sha1_hash>` with the actual SHA1 of your script.
*   Key names are listed first.
*   Arguments to the script follow a `--` separator.
*   Both keys and arguments are optional depending on what the script expects.

**Important Note on Persistence:**
The script cache (mapping SHA1 to compiled script) is held in memory by the `SlackbaseEngine`. If the server restarts, this cache is cleared. You will need to use `script load` (or other registration methods) again to make your scripts available after a restart. The `.lua` files in your `lua_scripts` directory serve as the persistent source for your scripts.

### Available Lua API (Inside Scripts)

Within a Lua script executed via `script run` or `evalsha`, the following are available:

*   `GET(key)`: Retrieves the value associated with `key`. Returns the value or `nil` if the key doesn't exist.
*   `SET(key, value)`: Sets the `key` to the given `value`.
*   `DEL(key)`: Deletes the `key`.
*   `KEYS`: A 1-indexed table containing the key names passed to `script run`/`evalsha`. (e.g., `KEYS[1]`, `KEYS[2]`)
*   `ARGV`: A 1-indexed table containing the argument values passed to `script run`/`evalsha`. (e.g., `ARGV[1]`, `ARGV[2]`)

### Lua Scripting Engine Internals (Deep Dive)

This section delves deeper into how the Lua scripting engine operates internally.

**Script Caching and Execution:**
*   When a script is first registered (e.g., via `script load` or `eval`), its source code is compiled into Lua bytecode by the `mlua` library.
*   This compiled bytecode (an `mlua::Function`) is then cached in memory within the `SlackbaseEngine`, associated with its SHA1 hash.
*   Subsequent calls to `script run` (or `evalsha`) with the same SHA1 hash will execute the cached bytecode directly, avoiding the overhead of recompilation. This makes repeated script executions very efficient.

**Bridging Lua and Rust:**
The real power comes from how Lua scripts interact with the underlying database engine:
*   When `script run` (or `evalsha`) is invoked, the Lua global environment is specially prepared for that execution instance.
*   The `GET`, `SET`, and `DEL` functions available in Lua are actually Rust closures.
*   These closures capture a raw pointer to the `SlackbaseEngine` instance. When `GET(key)` is called in Lua, it's invoking a Rust function that operates directly on the database, providing safe access to its methods. (This is achieved using `unsafe` Rust to bridge the gap, but the API exposed to Lua is safe).
*   Similarly, the `KEYS` and `ARGV` tables are populated directly from the arguments provided in the `script run`/`evalsha` command.

**Error Handling:**
*   Errors that occur during Lua script execution (e.g., syntax errors in the script, runtime errors, or errors from `GET/SET/DEL` operations failing within the Rust layer) are propagated back to the caller of `script run`/`evalsha`. The engine attempts to convert Lua-specific errors into its standard error types, and the CLI displays them.

**Execution Model & Atomicity:**
*   Lua scripts are executed within the engine's operational context. The execution of a single Lua script (including all the `GET`, `SET`, `DEL` calls it makes) is atomic from the perspective of other database commands. This means a script will run to completion without other commands interleaving its operations, assuming a single-threaded command processing model for the engine itself regarding script execution.

**Use Cases:**
Beyond simple batch operations, the Lua scripting engine enables:
*   **Atomic read-modify-write operations:** Retrieve a value, perform some computation on it, and write it back, all as a single, indivisible operation.
*   **Implementing custom commands:** Define complex database interactions tailored to your application's needs without modifying the core database engine.
*   **Conditional logic:** Perform different database operations based on the values of keys or arguments.

## Snapshot and Restore

Slackbase provides functionality to create snapshots of the database and restore from them. This is useful for backups and disaster recovery.

### Creating a Snapshot

A snapshot can be created using a command like `SNAPSHOT <path_to_snapshot_directory_or_prefix>` via the CLI:

```bash
slackbase> SNAPSHOT /mnt/backups/slackbase_backup_20231027
```

This command will copy the necessary database files (main data file, WAL file, hint file) to the specified location.

### Restoring from a Snapshot

To restore the database from a snapshot, you would use a command like `RESTORE <path_to_snapshot_directory_or_prefix>` via the CLI:

```bash
slackbase> RESTORE /mnt/backups/slackbase_backup_20231027
```

This will replace the current database files with the files from the snapshot and reload the database engine.

### **Important Note on Snapshots and Compaction**

The `COMPACT` command is used to rewrite the main database file, removing deleted and outdated entries to save space. While useful, it's crucial to observe the following precaution:

**Do NOT run `COMPACT` immediately before taking a `SNAPSHOT`.**

If the `COMPACT` operation is interrupted (e.g., due to a server crash or power loss), the main database file might be left in an incomplete or corrupted state. If you then take a `SNAPSHOT` of this compromised state, the snapshot will also be corrupted and potentially useless for recovery.

**Recommendation:** Ensure the database is in a stable and consistent state before initiating a `SNAPSHOT`. If you need to compact the database, do so at a time when you can verify its successful completion well before you plan to take a new snapshot.

## Other CLI Operations

The Slackbase CLI provides several other commands for interacting with the database:

*   **`PUT <key> <value>`**: Stores a key-value pair.
*   **`PUTEX <key> <value> <ttl_seconds>`**: Stores a key-value pair with a time-to-live (in seconds).
*   **`GET <key>`**: Retrieves the value for a key.
*   **`DEL <key>`**: Deletes a key.
*   **`COMPACT`**: Rewrites the database to reclaim space. (See snapshot warning above).
*   **`SCAN [PREFIX <prefix>]`**: Scans keys, optionally filtered by a prefix.
*   **`SCAN <start_key> <end_key>`**: Scans keys within a given range.
*   **`STATS`**: Shows database statistics.
*   **`BATCH put <k1> <v1> del <k2> ...`**: Allows for multiple PUT/DEL operations to be written to the WAL and applied as a single group.
*   **`EXIT` / `QUIT`**: Exits the Slackbase CLI.

The CLI will also prompt for a serialization format (`plain` or `json`) upon startup.
```
