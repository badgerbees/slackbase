# Slackbase

Slackbase is a key-value store database engine.

Note that I made this project as a way to learn Rust so it's not meant to be serious (hence why I call it "slack"-base) however I'm always open for feedbacks and suggestions.

## Features

*   **Key-Value Storage:** Store and retrieve data using simple key-value pairs.
*   **Complex Data Types:** Supports Hashes, Lists, Sets, and direct JSON object field manipulation.
*   **LRU Cache:** In-memory caching for frequently accessed keys to improve read performance.
*   **Secondary Indexing:** Index fields within JSON values for faster `find` queries.
*   **Persistence:** Data is saved to disk using an append-only log format.
*   **Write-Ahead Log (WAL):** Ensures data durability for write operations.
*   **In-Memory Indexing with Hint Files:** Fast key lookups with optimized startup times.
*   **Enhanced Lua Scripting:**
    *   Execute custom, atomic server-side scripts.
    *   Named scripts with descriptions.
    *   Persistent script metadata.
    *   CLI for loading, listing, running, renaming, and removing scripts.
*   **Snapshot and Restore:** Create backups and restore database state.
*   **Compaction:** Reclaim disk space by removing old/deleted data.
*   **Time-To-Live (TTL):** Optional automatic expiration for keys.
*   **CLI Interface:** Interactive command-line tool for all database operations.

## Slackbase Architecture

Slackbase is designed as a persistent key-value store with a focus on simplicity and durability. Here's an overview of its core components and processes:

### Core Components

*   **`SlackbaseEngine`:** This is the heart of the database. It manages:
    *   An **in-memory index (`HashMap`)**: Stores keys and their corresponding byte offset and length within the data file for quick lookups.
    *   An **LRU (Least Recently Used) Cache**: An in-memory cache (`LruCache`) to store frequently accessed key-value pairs, reducing disk I/O for common reads.
    *   A **Secondary Index**: Allows indexing of fields within JSON values, enabling faster queries based on specific JSON field content (e.g., using the `find` command).
    *   A **Write-Ahead Log (WAL)**: Ensures that write operations (`PUT`, `DEL`, and modifications to complex types) are durable. Changes are first written to the WAL.
    *   **Value Serialization**: Supports pluggable serializers (e.g., JSON, plain text). Internally, values are base64 encoded before being written to disk. For complex data types like Hashes, Lists, and Sets, the underlying storage is typically a JSON string.
    *   **Metrics Tracking**: Keeps track of operations like reads, writes, cache hits, and misses.
    *   **Lua Scripting Environment**: Manages Lua scripts, including their caching and execution.

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
1.  The value is serialized (if a serializer is configured) and then base64 encoded. This applies to simple values; complex types like Hashes, Lists, and Sets are typically serialized to a JSON string representation.
2.  The operation (e.g., `put key encoded_value expiry_timestamp`) is added to an in-memory write buffer.
3.  The buffer is flushed to the **Write-Ahead Log (`.wal` file)** for durability.
4.  The record is then appended to the main **data file (`.db` file)**.
5.  The **in-memory index** is updated with the new key's offset and length in the data file.
6.  The **LRU cache** is updated: if the key exists in the cache, its value is updated; if it's a new key, it may be added to the cache. If the operation is a deletion, the key is removed from the LRU cache.
7.  The **hint file (`.hint` file)** is updated with the new index information.
8.  If the operation involves a JSON value and secondary indexing is configured for affected fields, the **secondary index** is updated.

### Read Path
When a `GET` operation (or an internal read for complex types) occurs:
1.  The engine first checks the **LRU cache**. If the key is found and its value is cached, the value is returned immediately (cache hit), significantly speeding up the read.
2.  If the key is not in the LRU cache (cache miss), the engine consults the **in-memory index** for the key's offset and length.
3.  If found in the index, the data is read directly from the **data file (`.db` file)** at that specific location.
4.  If an expiry timestamp is present on the record, it's checked against the current time. Expired records are treated as if the key doesn't exist.
5.  The base64 encoded value is decoded and then deserialized (if a serializer is configured) before being returned.
6.  The retrieved value is then typically stored in the **LRU cache** for faster access in subsequent reads.

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

## Data Types

Beyond simple key-value strings, Slackbase supports several structured data types, primarily by storing and manipulating them as JSON strings under the hood. This allows for more complex data organization directly within the database.

### JSON Objects

*   **Description:** You can store arbitrary JSON objects as values. Specific fields within these JSON objects can be get or set directly.
*   **Storage:** Stored as a serialized JSON string.
*   **CLI Commands:**
    *   `PUT <key> <json_string>`: To store a full JSON object.
    *   `GET <key>`: To retrieve the full JSON object string.
    *   `JSON SET <key> <field> <value>`: Sets (or adds) a `field` to `value` within the JSON object at `key`. `value` itself can be a JSON primitive, array, or object.
    *   `JSON GET <key> <field>`: Retrieves the value of a `field` from the JSON object at `key`. The result is returned as a JSON string.
    *   `FIND <field_name> <value>`: Can be used with the secondary index to find keys where their JSON object value contains `field_name` equal to `value`.

### Hashes

*   **Description:** Hashes are maps of field-value pairs, conceptually similar to dictionaries or objects. They are ideal for representing objects where you need to frequently access or update individual fields.
*   **Storage:** Implemented as a JSON object.
*   **CLI Commands:**
    *   `HASH SET <key> <field> <value>`: Sets the `field` to `value` in the hash at `key`.
    *   `HASH GET <key> <field>`: Retrieves the value of `field` from the hash at `key`.
    *   `HASH DEL <key> <field>`: Deletes `field` from the hash at `key`.
    *   `HASH GETALL <key>`: Retrieves all field-value pairs from the hash at `key`.
    *   `DEL <key>`: Deletes the entire hash.

### Lists

*   **Description:** Lists are ordered sequences of strings. They support operations like push, pop, and range queries, making them suitable for queues, stacks, or timelines.
*   **Storage:** Implemented as a JSON array of strings.
*   **CLI Commands:**
    *   `LIST LPUSH <key> <value>`: Adds `value` to the beginning (left) of the list.
    *   `LIST RPUSH <key> <value>`: Adds `value` to the end (right) of the list.
    *   `LIST LPOP <key>`: Removes and returns the first element (left).
    *   `LIST RPOP <key>`: Removes and returns the last element (right).
    *   `LIST RANGE <key> <start> <end>`: Returns a sub-list of elements.
    *   `LIST LEN <key>`: Returns the number of elements in the list.
    *   `LIST SHOW <key>` (or `GET <key>`): Displays all elements in the list.
    *   `DEL <key>`: Deletes the entire list.

### Sets

*   **Description:** Sets are unordered collections of unique strings. Useful for tracking unique items, like tags or members.
*   **Storage:** Implemented as a JSON array of unique strings.
*   **CLI Commands:**
    *   `SET ADD <key> <value>`: Adds `value` to the set. If `value` already exists, the set remains unchanged.
    *   `SET SHOW <key>` (or `GET <key>`): Displays all elements in the set.
    *   `DEL <key>`: Deletes the entire set.

## Lua Scripting Engine

Slackbase supports server-side scripting using Lua. This allows for atomic execution of multiple operations and custom logic directly within the database.

The primary way to interact with Lua scripting is via the Slackbase Command Line Interface (CLI).

### Managing and Running Scripts via CLI

**1. Organizing Lua Script Files:**

It's recommended to place your Lua script files (e.g., `my_atomic_update.lua`) in a directory named `lua_scripts` located in the same directory where you run the `slackbase` executable. This directory serves as the persistent source for your scripts.

**2. Loading Scripts from Files:**

Use the `script load` command to register a script from a file into the engine. You can also provide a friendly name and an optional description:

```bash
slackbase> script load my_atomic_update.lua myUpdater "This script updates user profiles atomically."
Script 'myUpdater' cached, SHA1=abcdef1234567890abcdef1234567890abcdef12
```

This command reads `./lua_scripts/my_atomic_update.lua`, sends its content to the database engine for compilation and caching. The engine associates the script with the given name and its SHA1 hash.

**3. Interactive Script Input:**

For multi-line scripts directly in the CLI, you can use `script begin`. You can also provide a name and description here:

```bash
slackbase> script begin mySetter "Sets a key if it does not exist."
Enter Lua script. End with a line containing only END:
if GET(KEYS[1]) == nil then
  return SET(KEYS[1], ARGV[1])
else
  return nil
end
END
Script 'mySetter' cached, SHA1=fedcba0987654321fedcba0987654321fedcba09
```

**4. Direct Source Evaluation (`eval`):**

The `eval` command can still be used for quick, one-off script registration directly from source. It primarily returns the SHA1 hash.

```bash
slackbase> eval "return GET(KEYS[1])"
Script cached, SHA1=1234567890abcdef1234567890abcdef12345678
```
While `eval` registers the script, using `script load` or `script begin` is recommended for scripts you intend to reuse, as they allow assigning names and descriptions.

**5. Listing Registered Scripts:**

To see all scripts currently registered (cached) in the engine, use `script list`. This will now show the SHA1 hash, name, and description:

```bash
slackbase> script list
+------------------------------------------+------------------+--------------------------------------------------+
| SHA1                                     | Name             | Description                                      |
+------------------------------------------+------------------+--------------------------------------------------+
| abcdef1234567890abcdef1234567890abcdef12 | myUpdater        | This script updates user profiles atomically.    |
| fedcba0987654321fedcba0987654321fedcba09 | mySetter         | Sets a key if it does not exist.                 |
| 1234567890abcdef1234567890abcdef12345678 | 1234567890ab...  |                                                  |
+------------------------------------------+------------------+--------------------------------------------------+
```

**6. Running Registered Scripts:**

Once a script is registered, you can execute it using its SHA1 hash **or its assigned name** with the `script run` command (or its alias `evalsha`, which typically expects an SHA):

```bash
slackbase> script run <sha1_hash> [key1 key2 ...] -- [arg1 arg2 ...]
```
For example, to run a script with SHA1 `abcdef...`:
```bash
slackbase> script run abcdef1234567890abcdef1234567890abcdef12 user:1:profile -- name "New Name" age 30
Result: ... (output depends on the script)
```
*   Replace `<sha1_hash>` with the actual SHA1 or the *name* of your script (e.g., `myUpdater`).
*   Key names are listed first.
*   Arguments to the script follow a `--` separator.
*   Both keys and arguments are optional depending on what the script expects.

**7. Renaming Scripts:**

You can rename an existing script using `script rename`:
```bash
slackbase> script rename myUpdater userProfileUpdater
Script 'myUpdater' renamed to 'userProfileUpdater'
```

**8. Removing Scripts:**

Scripts can be removed from the cache using `script remove` with either their SHA1 hash or name:
```bash
slackbase> script remove userProfileUpdater
Script 'userProfileUpdater' removed.

slackbase> script remove fedcba0987654321fedcba0987654321fedcba09
Script 'fedcba0987654321fedcba0987654321fedcba09' removed.
```

**Important Note on Persistence:**
The script cache (mapping SHA1/name to compiled script and metadata) is held in memory by the `SlackbaseEngine`. Script metadata (name, SHA1, description) is persisted to a `.scripts` file (e.g., `slackbase.db.scripts`) when scripts are added or modified. However, the compiled Lua bytecode itself is not persisted in this file; it's recompiled from source if needed (though typically loaded from the `lua_scripts` directory or provided source on registration). If the server restarts, the engine attempts to reload script information from the `.scripts` file, but the actual script *source code* should still reside in your `lua_scripts` directory or be re-registered for the engine to fully restore them.

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
*   **`GET <key>`**: Retrieves the value for a key. This command is also used to display the content of Lists and Sets.
*   **`DEL <key>`**: Deletes a key (and its associated value, be it simple, Hash, List, or Set).
*   **`COMPACT`**: Rewrites the database to reclaim space. (See snapshot warning above).
*   **`SCAN [PREFIX <prefix>]`**: Scans keys, optionally filtered by a prefix.
*   **`SCAN <start_key> <end_key>`**: Scans keys within a given range.
*   **`STATS`**: Shows database statistics (including LRU cache performance).
*   **`BATCH put <k1> <v1> del <k2> ...`**: Allows for multiple PUT/DEL operations to be written to the WAL and applied as a single group.
*   **`FIND <field_name> <value>`**: Searches for keys where a JSON value contains the given field with the specified value. Requires the secondary index.

*   **JSON Operations:**
    *   **`JSON SET <key> <field> <json_value>`**: Sets a specific `field` within a JSON object stored at `key` to `json_value`. If `key` doesn't exist or isn't a JSON object, it's created/overwritten.
    *   **`JSON GET <key> <field>`**: Retrieves the value of a specific `field` from a JSON object stored at `key`.

*   **List Operations (FIFO/LIFO):**
    *   **`LIST LPUSH <key> <value>`**: Prepends `value` to the list stored at `key`. Creates the list if it doesn't exist.
    *   **`LIST RPUSH <key> <value>`**: Appends `value` to the list stored at `key`. Creates the list if it doesn't exist. (Note: `LIST PUSH` might be an alias or older version of `RPUSH`).
    *   **`LIST LPOP <key>`**: Removes and returns the first element from the list at `key`.
    *   **`LIST RPOP <key>`**: Removes and returns the last element from thelist at `key`.
    *   **`LIST RANGE <key> <start_index> <end_index>`**: Returns a range of elements from the list at `key`. Indexes can be negative (from end).
    *   **`LIST LEN <key>`**: Returns the length of the list at `key`.
    *   **`LIST SHOW <key>`**: Displays the entire list (often an alias for `GET <key>`).

*   **Set Operations (Unordered, Unique Elements):**
    *   **`SET ADD <key> <value>`**: Adds `value` to the set stored at `key`. If `value` already exists, it's ignored. Creates the set if it doesn't exist.
    *   **`SET SHOW <key>`**: Displays all elements in the set (often an alias for `GET <key>`).

*   **Hash Operations (Key-Field-Value Maps):**
    *   **`HASH SET <key> <field> <value>`**: Sets the `field` to `value` within the hash stored at `key`. Creates the hash if it doesn't exist.
    *   **`HASH GET <key> <field>`**: Retrieves the `value` of `field` from the hash at `key`.
    *   **`HASH DEL <key> <field>`**: Deletes `field` from the hash at `key`.
    *   **`HASH GETALL <key>`**: Retrieves all field-value pairs from the hash at `key`.

*   **Lua Scripting Commands:**
    *   **`SCRIPT LOAD <filepath> <name> [description]`**: Loads a Lua script from `<filepath>`, assigns it a `<name>`, and optionally a `[description]`.
    *   **`SCRIPT BEGIN <name> [description]`**: Starts interactive input for a new Lua script, assigning it a `<name>` and optionally a `[description]`.
    *   **`SCRIPT LIST`**: Lists all cached scripts with their SHA1, name, and description.
    *   **`SCRIPT RUN <sha1_or_name> [key1 key2 ...] -- [arg1 arg2 ...]`**: Executes a cached script identified by its SHA1 hash or name.
    *   **`SCRIPT RENAME <old_name> <new_name>`**: Renames a cached script.
    *   **`SCRIPT REMOVE <sha1_or_name>`**: Removes a script from the cache.
    *   **`EVAL <lua_source>`**: Compiles and caches a Lua script directly from source string (primarily for quick tests).
    *   **`EVALSHA <sha1> [key1 key2 ...] -- [arg1 arg2 ...]`**: Executes a cached script by its SHA1 hash (similar to `SCRIPT RUN` with SHA1).

*   **`EXIT` / `QUIT`**: Exits the Slackbase CLI.

The CLI will also prompt for a serialization format (`plain` or `json`) upon startup. This choice affects how raw string values are interpreted by default, though complex types like Hashes, Lists, and Sets internally use JSON representation.
```
