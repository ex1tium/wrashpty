# Command Intelligence Engine - Unified Specification

> A comprehensive system for learning command patterns and providing context-aware intelligent suggestions in wrashpty.

## Executive Summary

This specification defines a full-featured Command Intelligence Engine that learns from command history to provide intelligent, context-aware suggestions. The system includes pattern learning, session context tracking, template recognition, fuzzy search, user customization, and cross-machine pattern sharing.

**Key Principle**: Build incrementally on existing infrastructure rather than replacing it.

---

## Existing Infrastructure (Leverage Points)

The codebase already provides solid foundations:

| Component | Location | What It Provides |
|-----------|----------|------------------|
| `TokenType` enum | `command_edit.rs:20-35` | Token classification (Command, Subcommand, Flag, Path, etc.) |
| `CommandToken` | `command_edit.rs:46-74` | Token structure with text, type, locked state |
| `classify_token()` | `command_edit.rs:77-99` | Position and content-based classification |
| `COMMAND_KNOWLEDGE` | `command_knowledge.rs` | Static subcommand/filetype knowledge |
| `HistoryStore` | `history_store.rs` | SQLite with reedline, metadata (exit_status, cwd, duration) |
| `tokens_at_position()` | `history_store.rs:583-640` | Historical token frequency queries |
| `CommandEditState` | `command_edit.rs:347-854` | Unified editing engine with suggestions |

---

## Feature Overview

### Core Features

1. **Token Sequence Learning** - What follows what (`git commit` → `-m`)
2. **Pipe Chain Patterns** - Post-pipe suggestions (`cat *.log |` → `grep`)
3. **Flag Value Memory** - Common flag values (`docker run -p` → `8080:8080`)
4. **Frecency Ranking** - Frequency × recency × success rate scoring

### Advanced Features

5. **Session Context Tracking** - Learn command sequences within terminal sessions
6. **Command Templates** - Abstract patterns with placeholders (`docker run -p <PORT>:<PORT> <IMAGE>`)
7. **Failure Learning** - Prefer successful command variants over failed ones
8. **FTS5 Fuzzy Search** - Typo tolerance and partial matching
9. **User-Defined Patterns** - Custom aliases and suggestion rules
10. **Export/Import** - Share learned patterns across machines

---

## Architecture Overview

```
┌─────────────────────────────────────────────────────────────────────────┐
│                            UI Layer                                      │
│  ┌─────────────────┐  ┌─────────────────┐  ┌─────────────────────────┐  │
│  │ HistoryBrowser  │  │  FileBrowser    │  │    CommandPalette       │  │
│  │     Panel       │  │    Panel        │  │       Panel             │  │
│  └────────┬────────┘  └────────┬────────┘  └───────────┬─────────────┘  │
│           │                    │                        │                │
│           └────────────────────┼────────────────────────┘                │
│                                │                                         │
│  ┌─────────────────────────────▼─────────────────────────────────────┐  │
│  │                   CommandEditState (Shared)                        │  │
│  │  - Token management, editing, undo, danger check                   │  │
│  │  - Suggestion display and cycling                                  │  │
│  └─────────────────────────────┬─────────────────────────────────────┘  │
│                                │                                         │
└────────────────────────────────┼────────────────────────────────────────┘
                                 │
┌────────────────────────────────┼────────────────────────────────────────┐
│                       Intelligence Layer                                 │
│                                │                                         │
│  ┌─────────────────────────────▼─────────────────────────────────────┐  │
│  │                    SuggestionEngine                                │  │
│  │  - Aggregates all suggestion sources                               │  │
│  │  - Ranks by frecency, context, success rate                        │  │
│  │  - Applies user pattern overrides                                  │  │
│  │  - Falls back through: UserPattern → Learned → FTS5 → Static       │  │
│  └─────────────────────────────┬─────────────────────────────────────┘  │
│                                │                                         │
│  ┌─────────────────────────────┼─────────────────────────────────────┐  │
│  │     ┌───────────────────────┼───────────────────────┐             │  │
│  │     │                       │                       │             │  │
│  │  ┌──▼──────────┐  ┌─────────▼───────┐  ┌───────────▼──────────┐  │  │
│  │  │ PatternDB   │  │ SessionTracker  │  │   TemplateEngine     │  │  │
│  │  │ - sequences │  │ - transitions   │  │   - placeholders     │  │  │
│  │  │ - pipes     │  │ - session cmds  │  │   - value history    │  │  │
│  │  │ - flags     │  │ - time deltas   │  │   - template match   │  │  │
│  │  └─────────────┘  └─────────────────┘  └──────────────────────┘  │  │
│  │                                                                   │  │
│  │  ┌───────────────┐  ┌───────────────┐  ┌──────────────────────┐  │  │
│  │  │ FuzzySearch   │  │ UserPatterns  │  │   ExportImport       │  │  │
│  │  │ - FTS5        │  │ - aliases     │  │   - JSON format      │  │  │
│  │  │ - typo fix    │  │ - triggers    │  │   - merge/replace    │  │  │
│  │  │ - partial     │  │ - priority    │  │   - anonymize        │  │  │
│  │  └───────────────┘  └───────────────┘  └──────────────────────┘  │  │
│  │                                                                   │  │
│  └───────────────────────────────────────────────────────────────────┘  │
│                                                                          │
└──────────────────────────────────────────────────────────────────────────┘
                                 │
┌────────────────────────────────┼────────────────────────────────────────┐
│                        Storage Layer                                     │
│                                │                                         │
│  ┌─────────────────────────────▼─────────────────────────────────────┐  │
│  │                   SQLite (Shared Database)                         │  │
│  │  ┌─────────────────────────────────────────────────────────────┐  │  │
│  │  │ Reedline Tables (READ ONLY)                                 │  │  │
│  │  │  - history (command_line, start_timestamp, ...)             │  │  │
│  │  └─────────────────────────────────────────────────────────────┘  │  │
│  │  ┌─────────────────────────────────────────────────────────────┐  │  │
│  │  │ Intelligence Tables (READ/WRITE) - ci_* prefix              │  │  │
│  │  │  - ci_tokens, ci_commands, ci_sequences, ci_pipe_chains     │  │  │
│  │  │  - ci_flag_values, ci_sessions, ci_transitions              │  │  │
│  │  │  - ci_templates, ci_template_values, ci_command_variants    │  │  │
│  │  │  - ci_user_patterns, ci_user_aliases                        │  │  │
│  │  │  - ci_commands_fts (FTS5 virtual table)                     │  │  │
│  │  └─────────────────────────────────────────────────────────────┘  │  │
│  └───────────────────────────────────────────────────────────────────┘  │
│                                                                          │
└──────────────────────────────────────────────────────────────────────────┘
```

---

## Module Structure

```
src/
├── intelligence/                        # Command Intelligence Engine
│   ├── mod.rs                           # Module root, CommandIntelligence struct
│   ├── types.rs                         # Core types (Suggestion, Context, etc.)
│   ├── error.rs                         # CIError type
│   │
│   ├── schema.rs                        # Database schema creation/migration
│   ├── sync.rs                          # Incremental sync from reedline
│   ├── tokenizer.rs                     # Enhanced token analysis
│   │
│   ├── patterns/                        # Pattern learning subsystem
│   │   ├── mod.rs                       # Pattern learning coordination
│   │   ├── sequences.rs                 # Token sequence patterns
│   │   ├── pipes.rs                     # Pipe chain patterns
│   │   └── flags.rs                     # Flag-value associations
│   │
│   ├── sessions.rs                      # Session context tracking
│   ├── templates.rs                     # Template extraction and matching
│   ├── variants.rs                      # Success/failure variant tracking
│   ├── fuzzy.rs                         # FTS5 fuzzy search
│   │
│   ├── suggest.rs                       # Main suggestion engine
│   ├── scoring.rs                       # Frecency and ranking algorithms
│   │
│   ├── user_patterns.rs                 # User-defined patterns and aliases
│   └── export.rs                        # Export/import functionality
│
├── chrome/
│   ├── command_edit.rs                  # EXTEND: build_context() method
│   ├── command_knowledge.rs             # EXISTING: Static knowledge (unchanged)
│   ├── history_browser.rs               # EXTEND: Use intelligent suggestions
│   └── file_browser.rs                  # EXTEND: Use intelligent suggestions
│
└── history_store.rs                     # EXTEND: Integrate CommandIntelligence
```

---

## Database Schema

All intelligence tables use `ci_` prefix to avoid conflicts with reedline.

### Core Tables

```sql
-- Schema version for migrations
CREATE TABLE IF NOT EXISTS ci_schema_version (
    version INTEGER PRIMARY KEY,
    applied_at INTEGER NOT NULL
);

-- Sync state tracking
CREATE TABLE IF NOT EXISTS ci_sync_state (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL
);

-- Token vocabulary (deduplicated, classified)
CREATE TABLE IF NOT EXISTS ci_tokens (
    id INTEGER PRIMARY KEY,
    text TEXT NOT NULL UNIQUE,
    token_type TEXT NOT NULL,            -- Command, Subcommand, Flag, Path, etc.
    frequency INTEGER DEFAULT 1,
    first_seen INTEGER NOT NULL,
    last_seen INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_ci_tokens_text ON ci_tokens(text);
CREATE INDEX IF NOT EXISTS idx_ci_tokens_type ON ci_tokens(token_type);
CREATE INDEX IF NOT EXISTS idx_ci_tokens_freq ON ci_tokens(frequency DESC);

-- Processed command metadata
CREATE TABLE IF NOT EXISTS ci_commands (
    id INTEGER PRIMARY KEY,
    reedline_id INTEGER UNIQUE,          -- Reference to history.id
    command_line TEXT NOT NULL,          -- Full command for FTS
    command_hash TEXT NOT NULL,          -- For dedup detection
    token_ids TEXT NOT NULL,             -- JSON array of ci_tokens.id
    token_count INTEGER NOT NULL,
    base_command_id INTEGER,             -- ci_tokens.id of first token
    exit_status INTEGER,
    cwd TEXT,
    timestamp INTEGER NOT NULL,
    session_id INTEGER,                  -- ci_sessions.id (nullable)
    FOREIGN KEY (base_command_id) REFERENCES ci_tokens(id),
    FOREIGN KEY (session_id) REFERENCES ci_sessions(id) ON DELETE SET NULL
);
CREATE INDEX IF NOT EXISTS idx_ci_commands_hash ON ci_commands(command_hash);
CREATE INDEX IF NOT EXISTS idx_ci_commands_base ON ci_commands(base_command_id);
CREATE INDEX IF NOT EXISTS idx_ci_commands_ts ON ci_commands(timestamp DESC);
CREATE INDEX IF NOT EXISTS idx_ci_commands_session ON ci_commands(session_id);
```

### Pattern Learning Tables

```sql
-- Token sequences: what follows what in which context
CREATE TABLE IF NOT EXISTS ci_sequences (
    id INTEGER PRIMARY KEY,
    context_token_id INTEGER NOT NULL,   -- The preceding token
    context_position INTEGER NOT NULL,   -- Position in command (0, 1, 2, ...)
    base_command_id INTEGER,             -- The base command (git, docker, etc.)
    base_command_key INTEGER GENERATED ALWAYS AS (COALESCE(base_command_id, -1)) STORED,
    next_token_id INTEGER NOT NULL,      -- What comes next
    frequency INTEGER DEFAULT 1,
    success_count INTEGER DEFAULT 0,
    last_seen INTEGER NOT NULL,
    UNIQUE(context_token_id, context_position, base_command_key, next_token_id),
    FOREIGN KEY (context_token_id) REFERENCES ci_tokens(id),
    FOREIGN KEY (base_command_id) REFERENCES ci_tokens(id),
    FOREIGN KEY (next_token_id) REFERENCES ci_tokens(id)
);
CREATE INDEX IF NOT EXISTS idx_ci_seq_context ON ci_sequences(context_token_id, base_command_id);
CREATE INDEX IF NOT EXISTS idx_ci_seq_freq ON ci_sequences(frequency DESC);

-- N-gram patterns (2-gram, 3-gram)
CREATE TABLE IF NOT EXISTS ci_ngrams (
    id INTEGER PRIMARY KEY,
    n INTEGER NOT NULL,                  -- 2 or 3
    pattern_hash TEXT NOT NULL UNIQUE,   -- Hash of token sequence
    token_ids TEXT NOT NULL,             -- JSON array of context tokens
    next_token_id INTEGER NOT NULL,
    frequency INTEGER DEFAULT 1,
    last_seen INTEGER NOT NULL,
    FOREIGN KEY (next_token_id) REFERENCES ci_tokens(id)
);
CREATE INDEX IF NOT EXISTS idx_ci_ngrams_hash ON ci_ngrams(pattern_hash);

-- Pipe chains: what commands follow pipes
CREATE TABLE IF NOT EXISTS ci_pipe_chains (
    id INTEGER PRIMARY KEY,
    pre_pipe_base_cmd_id INTEGER,        -- Base command before pipe
    pre_pipe_hash TEXT NOT NULL,         -- Hash of full pre-pipe segment
    pipe_command_id INTEGER NOT NULL,    -- First token after pipe
    full_chain TEXT,                     -- JSON: complete post-pipe command
    chain_length INTEGER DEFAULT 1,
    frequency INTEGER DEFAULT 1,
    last_seen INTEGER NOT NULL,
    UNIQUE(pre_pipe_hash, pipe_command_id),
    FOREIGN KEY (pre_pipe_base_cmd_id) REFERENCES ci_tokens(id),
    FOREIGN KEY (pipe_command_id) REFERENCES ci_tokens(id)
);
CREATE INDEX IF NOT EXISTS idx_ci_pipes_hash ON ci_pipe_chains(pre_pipe_hash);
CREATE INDEX IF NOT EXISTS idx_ci_pipes_freq ON ci_pipe_chains(frequency DESC);

-- Flag values: common values for specific flags
CREATE TABLE IF NOT EXISTS ci_flag_values (
    id INTEGER PRIMARY KEY,
    base_command_id INTEGER NOT NULL,
    subcommand_id INTEGER,               -- Nullable for commands without subcommands
    subcommand_key INTEGER GENERATED ALWAYS AS (COALESCE(subcommand_id, -1)) STORED,
    flag_text TEXT NOT NULL,
    value_text TEXT NOT NULL,
    value_type TEXT,                     -- port, path, number, url, etc.
    frequency INTEGER DEFAULT 1,
    last_seen INTEGER NOT NULL,
    UNIQUE(base_command_id, subcommand_key, flag_text, value_text),
    FOREIGN KEY (base_command_id) REFERENCES ci_tokens(id),
    FOREIGN KEY (subcommand_id) REFERENCES ci_tokens(id)
);
CREATE INDEX IF NOT EXISTS idx_ci_flags_cmd ON ci_flag_values(base_command_id, flag_text);
```

### Session Tracking Tables

```sql
-- Terminal sessions
-- Sessions have a last_activity timestamp for inactivity-based timeout
CREATE TABLE IF NOT EXISTS ci_sessions (
    id INTEGER PRIMARY KEY,
    session_id TEXT NOT NULL UNIQUE,     -- UUID per terminal session
    start_time INTEGER NOT NULL,
    end_time INTEGER,                    -- NULL while active, set by end_session()
    last_activity INTEGER NOT NULL,      -- Updated on each command; used for timeout
    command_count INTEGER DEFAULT 0,
    cwd_at_start TEXT
);
CREATE INDEX IF NOT EXISTS idx_ci_sessions_uuid ON ci_sessions(session_id);
CREATE INDEX IF NOT EXISTS idx_ci_sessions_activity ON ci_sessions(last_activity);

-- Commands within sessions (for sequence learning)
CREATE TABLE IF NOT EXISTS ci_session_commands (
    id INTEGER PRIMARY KEY,
    session_id INTEGER NOT NULL,
    sequence_number INTEGER NOT NULL,    -- Order within session
    command_id INTEGER NOT NULL,
    timestamp INTEGER NOT NULL,
    UNIQUE(session_id, sequence_number),
    FOREIGN KEY (session_id) REFERENCES ci_sessions(id),
    FOREIGN KEY (command_id) REFERENCES ci_commands(id)
);
CREATE INDEX IF NOT EXISTS idx_ci_session_cmds ON ci_session_commands(session_id, sequence_number);

-- Command-to-command transitions
CREATE TABLE IF NOT EXISTS ci_transitions (
    id INTEGER PRIMARY KEY,
    from_command_hash TEXT NOT NULL,
    to_command_hash TEXT NOT NULL,
    from_base_cmd_id INTEGER,
    to_base_cmd_id INTEGER,
    frequency INTEGER DEFAULT 1,
    avg_time_delta INTEGER,              -- Seconds between commands
    last_seen INTEGER NOT NULL,
    UNIQUE(from_command_hash, to_command_hash),
    FOREIGN KEY (from_base_cmd_id) REFERENCES ci_tokens(id),
    FOREIGN KEY (to_base_cmd_id) REFERENCES ci_tokens(id)
);
CREATE INDEX IF NOT EXISTS idx_ci_trans_from ON ci_transitions(from_command_hash);
CREATE INDEX IF NOT EXISTS idx_ci_trans_base ON ci_transitions(from_base_cmd_id);
```

#### Session Lifecycle and Retention

**Session Management:**
- `start_session(session_id)`: Creates a new session with `start_time = now`, `last_activity = now`
- `track_session_command()`: Updates `last_activity = now` and increments `command_count`
- `end_session()`: Sets `end_time = now` for graceful termination

**Timeout and Retention Policies:**

| Policy | Default Value | Description |
|--------|---------------|-------------|
| Inactivity timeout | 24 hours | Sessions with `last_activity` older than this are auto-closed |
| Session retention | 90 days | Sessions older than this (by `end_time`) are purged |
| Orphan cleanup | 7 days | Sessions without `end_time` and `last_activity` > 7 days are force-closed |

**Maintenance Task (run periodically, e.g., on startup or daily):**

```sql
-- 1. Auto-close stale sessions (24h inactivity timeout)
UPDATE ci_sessions
SET end_time = last_activity
WHERE end_time IS NULL
  AND last_activity < (strftime('%s', 'now') - 86400);

-- 2. Purge old sessions and related commands (90-day retention)
DELETE FROM ci_session_commands
WHERE session_id IN (
    SELECT id FROM ci_sessions
    WHERE end_time IS NOT NULL
      AND end_time < (strftime('%s', 'now') - 7776000)
);
DELETE FROM ci_sessions
WHERE end_time IS NOT NULL
  AND end_time < (strftime('%s', 'now') - 7776000);
```

These operations are idempotent and safe to run concurrently.

### Template Tables

```sql
-- Recognized command templates with placeholders
CREATE TABLE IF NOT EXISTS ci_templates (
    id INTEGER PRIMARY KEY,
    template TEXT NOT NULL UNIQUE,       -- "docker run -p <PORT>:<PORT> <IMAGE>"
    template_hash TEXT NOT NULL,
    base_command_id INTEGER,
    placeholder_count INTEGER NOT NULL,
    placeholders TEXT NOT NULL,          -- JSON: [{"name": "PORT", "type": "port", "position": 3}]
    frequency INTEGER DEFAULT 1,
    last_seen INTEGER NOT NULL,
    example_command TEXT,                -- Concrete example for preview
    FOREIGN KEY (base_command_id) REFERENCES ci_tokens(id)
);
CREATE INDEX IF NOT EXISTS idx_ci_templates_hash ON ci_templates(template_hash);
CREATE INDEX IF NOT EXISTS idx_ci_templates_cmd ON ci_templates(base_command_id);

-- Historical values for template placeholders
CREATE TABLE IF NOT EXISTS ci_template_values (
    id INTEGER PRIMARY KEY,
    template_id INTEGER NOT NULL,
    placeholder_name TEXT NOT NULL,
    value_text TEXT NOT NULL,
    value_type TEXT,
    frequency INTEGER DEFAULT 1,
    last_seen INTEGER NOT NULL,
    UNIQUE(template_id, placeholder_name, value_text),
    FOREIGN KEY (template_id) REFERENCES ci_templates(id)
);
CREATE INDEX IF NOT EXISTS idx_ci_tpl_vals ON ci_template_values(template_id, placeholder_name);
```

### Failure Learning Tables

```sql
-- Command variants with success/failure tracking
CREATE TABLE IF NOT EXISTS ci_command_variants (
    id INTEGER PRIMARY KEY,
    canonical_pattern TEXT NOT NULL,     -- Normalized pattern
    variant_hash TEXT NOT NULL,
    variant_command TEXT NOT NULL,
    success_count INTEGER DEFAULT 0,
    failure_count INTEGER DEFAULT 0,
    last_success INTEGER,
    last_failure INTEGER,
    success_rate REAL GENERATED ALWAYS AS (
        CAST(success_count AS REAL) / NULLIF(success_count + failure_count, 0)
    ) STORED,
    UNIQUE(canonical_pattern, variant_hash)
);
CREATE INDEX IF NOT EXISTS idx_ci_variants_pattern ON ci_command_variants(canonical_pattern);
CREATE INDEX IF NOT EXISTS idx_ci_variants_success ON ci_command_variants(success_rate DESC);
```

### FTS5 Full-Text Search

```sql
-- Virtual table for fuzzy search
CREATE VIRTUAL TABLE IF NOT EXISTS ci_commands_fts USING fts5(
    command_line,
    base_command,
    content='ci_commands',
    content_rowid='id',
    tokenize='porter unicode61'
);

-- Triggers to keep FTS in sync
CREATE TRIGGER IF NOT EXISTS ci_commands_fts_ai AFTER INSERT ON ci_commands BEGIN
    INSERT INTO ci_commands_fts(rowid, command_line, base_command)
    SELECT new.id, new.command_line,
           (SELECT text FROM ci_tokens WHERE id = new.base_command_id);
END;

CREATE TRIGGER IF NOT EXISTS ci_commands_fts_ad AFTER DELETE ON ci_commands BEGIN
    INSERT INTO ci_commands_fts(ci_commands_fts, rowid, command_line, base_command)
    VALUES('delete', old.id, old.command_line,
           (SELECT text FROM ci_tokens WHERE id = old.base_command_id));
END;

CREATE TRIGGER IF NOT EXISTS ci_commands_fts_au AFTER UPDATE ON ci_commands BEGIN
    INSERT INTO ci_commands_fts(ci_commands_fts, rowid, command_line, base_command)
    VALUES('delete', old.id, old.command_line,
           (SELECT text FROM ci_tokens WHERE id = old.base_command_id));
    INSERT INTO ci_commands_fts(rowid, command_line, base_command)
    SELECT new.id, new.command_line,
           (SELECT text FROM ci_tokens WHERE id = new.base_command_id);
END;
```

#### FTS5 Maintenance and Recovery

**Initial Population:**

Triggers are atomic via SQLite but do NOT backfill existing rows. After creating
`ci_commands_fts`, immediately rebuild to populate from existing `ci_commands`:

```sql
INSERT INTO ci_commands_fts(ci_commands_fts) VALUES('rebuild');
```

**Integrity Check and Recovery:**

If FTS index corruption is suspected, run an integrity check:

```sql
INSERT INTO ci_commands_fts(ci_commands_fts) VALUES('integrity-check');
```

If errors are returned, rebuild the index:

```sql
INSERT INTO ci_commands_fts(ci_commands_fts) VALUES('rebuild');
```

**Query Fallback:**

If an FTS query fails (e.g., due to corruption or syntax error), the fuzzy search
implementation should catch the error and fall back to a safe LIKE-based query:

```rust
// In fuzzy.rs
fn fuzzy_search_fallback(conn: &Connection, query: &str, limit: usize) -> Vec<FuzzyMatch> {
    // Fallback to LIKE when FTS fails
    // Escape SQL LIKE wildcards to prevent unsafe matching
    let escaped = query
        .replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_");
    let pattern = format!("%{}%", escaped);
    conn.prepare(
        "SELECT command_line FROM ci_commands WHERE command_line LIKE ?1 ESCAPE '\\' LIMIT ?2"
    )
    .and_then(|mut stmt| {
        stmt.query_map(params![&pattern, limit as i64], |row| /* ... */)
    })
    .unwrap_or_default()
}
```

**Bulk Import Guidance:**

For large imports (e.g., migration or restore):

1. Disable automerge for faster inserts:
   ```sql
   INSERT INTO ci_commands_fts(ci_commands_fts, rank) VALUES('automerge', 0);
   ```

2. Insert data into `ci_commands` in batched transactions (1000 rows per batch)

3. After all inserts, optimize the FTS index:
   ```sql
   INSERT INTO ci_commands_fts(ci_commands_fts) VALUES('optimize');
   ```

4. Re-enable automerge:
   ```sql
   INSERT INTO ci_commands_fts(ci_commands_fts, rank) VALUES('automerge', 4);
   ```

5. Final rebuild to ensure consistency:
   ```sql
   INSERT INTO ci_commands_fts(ci_commands_fts) VALUES('rebuild');
   ```

### User Pattern Tables

```sql
-- User-defined custom patterns
CREATE TABLE IF NOT EXISTS ci_user_patterns (
    id INTEGER PRIMARY KEY,
    pattern_type TEXT NOT NULL,          -- alias, sequence, file_type, trigger
    trigger_pattern TEXT NOT NULL,       -- What activates this pattern
    suggestion TEXT NOT NULL,            -- What to suggest
    description TEXT,                    -- User-provided description
    priority INTEGER DEFAULT 0,          -- Higher = shown first
    enabled INTEGER DEFAULT 1,
    created_at INTEGER NOT NULL,
    last_used INTEGER,
    use_count INTEGER DEFAULT 0
);
CREATE INDEX IF NOT EXISTS idx_ci_user_patterns_trigger ON ci_user_patterns(trigger_pattern, enabled);
CREATE INDEX IF NOT EXISTS idx_ci_user_patterns_type ON ci_user_patterns(pattern_type, enabled);

-- User-defined aliases (special case of patterns)
CREATE TABLE IF NOT EXISTS ci_user_aliases (
    id INTEGER PRIMARY KEY,
    alias TEXT NOT NULL UNIQUE,          -- Short name
    expansion TEXT NOT NULL,             -- Full command expansion
    description TEXT,
    enabled INTEGER DEFAULT 1,
    created_at INTEGER NOT NULL,
    last_used INTEGER,
    use_count INTEGER DEFAULT 0
);
CREATE INDEX IF NOT EXISTS idx_ci_user_aliases_name ON ci_user_aliases(alias, enabled);
```

### Caching Tables

```sql
-- Suggestion cache for performance
CREATE TABLE IF NOT EXISTS ci_suggestion_cache (
    cache_key TEXT PRIMARY KEY,
    suggestions TEXT NOT NULL,           -- JSON array of suggestions
    computed_at INTEGER NOT NULL,
    expires_at INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_ci_cache_expires ON ci_suggestion_cache(expires_at);
```

#### Cache Management Plan

**TTL Strategy:**

All cache entries use a fixed TTL of **300 seconds (5 minutes)**. The `expires_at`
value is computed as `computed_at + 300`.

```rust
let expires_at = chrono::Utc::now().timestamp() + 300; // 5-minute TTL
```

**Cache Key Format:**

```
{position_type}:{base_command}:{partial}:{cwd_hash}
```

Example: `Subcommand:git:co:/home/user_abc123`

**Eviction Policies:**

| Policy | Rule | Implementation |
|--------|------|----------------|
| Time-based | Delete rows where `expires_at < now()` | Periodic cleanup |
| Max size | Limit to 10,000 entries | LRU eviction when exceeded |
| Max per-context | Limit to 100 entries per base_command | Prevents cache bloat |

**Periodic Cleanup (run on sync or every 60 seconds):**

```sql
-- Remove expired entries
DELETE FROM ci_suggestion_cache WHERE expires_at < strftime('%s', 'now');

-- LRU eviction: keep only most recent 10,000 entries
DELETE FROM ci_suggestion_cache
WHERE cache_key NOT IN (
    SELECT cache_key FROM ci_suggestion_cache
    ORDER BY computed_at DESC
    LIMIT 10000
);
```

**Invalidation Points:**

Cache entries are invalidated (deleted or refreshed) when:

1. **Command Learning:** After `learn_command()` completes, invalidate cache entries
   matching the learned command's base command:
   ```sql
   DELETE FROM ci_suggestion_cache WHERE cache_key LIKE '%:{base_command}:%';
   ```

2. **Pattern Updates:** After user pattern CRUD operations, invalidate all cache:
   ```sql
   DELETE FROM ci_suggestion_cache;
   ```

3. **Session Changes:** On `start_session()` or `end_session()`, invalidate
   session-dependent entries (those with `SessionTransition` source)

4. **Import/Export:** After `import()`, clear entire cache to reflect new patterns

---

## Core Types

### Suggestion Types

```rust
/// A ranked suggestion from any source
#[derive(Debug, Clone)]
pub struct Suggestion {
    /// The suggested text
    pub text: String,

    /// Where this suggestion came from
    pub source: SuggestionSource,

    /// Computed relevance score (0.0 - 1.0+)
    pub score: f64,

    /// Additional context for display
    pub metadata: SuggestionMetadata,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SuggestionSource {
    /// From static COMMAND_KNOWLEDGE
    StaticKnowledge,

    /// From learned token sequences
    LearnedSequence,

    /// From learned pipe patterns
    LearnedPipe,

    /// From learned flag values
    LearnedFlagValue,

    /// From session transition patterns
    SessionTransition,

    /// From template completion
    Template,

    /// From FTS5 fuzzy search
    FuzzySearch,

    /// From historical frequency
    HistoricalFrequency,

    /// User-defined pattern
    UserPattern,

    /// User-defined alias
    UserAlias,
}

#[derive(Debug, Clone, Default)]
pub struct SuggestionMetadata {
    /// How many times this pattern was seen
    pub frequency: u32,

    /// Success rate (0.0 - 1.0) if known
    pub success_rate: Option<f64>,

    /// When last used (unix timestamp)
    pub last_seen: Option<i64>,

    /// For templates: the filled preview
    pub template_preview: Option<String>,

    /// For fuzzy: the match quality
    pub fuzzy_score: Option<f64>,

    /// User-provided description
    pub description: Option<String>,
}
```

### Context Types

```rust
/// Context for generating suggestions
#[derive(Debug, Clone)]
pub struct SuggestionContext {
    /// Tokens before the current edit position
    pub preceding_tokens: Vec<AnalyzedToken>,

    /// The partial text being typed
    pub partial: String,

    /// Current working directory
    pub cwd: Option<PathBuf>,

    /// Position type for specialized suggestions
    pub position: PositionType,

    /// File context if in file browser
    pub file_context: Option<FileContext>,

    /// Current session for transition suggestions
    pub session: Option<SessionContext>,

    /// Last executed command (for "next" suggestions)
    pub last_command: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PositionType {
    /// First token position (command)
    Command,

    /// After a known command (subcommand)
    Subcommand,

    /// After a flag that expects a value
    FlagValue { flag: &str },

    /// After a pipe operator
    AfterPipe,

    /// Generic argument position
    Argument,

    /// After redirect (>, >>)
    AfterRedirect,
}

#[derive(Debug, Clone)]
pub struct FileContext {
    pub filename: String,
    pub extension: Option<String>,
    pub is_directory: bool,
}

#[derive(Debug, Clone)]
pub struct SessionContext {
    pub session_id: String,
    pub recent_commands: Vec<String>,    // Last N commands in session
    pub command_count: u32,
}

/// An analyzed token with classification
#[derive(Debug, Clone)]
pub struct AnalyzedToken {
    pub text: String,
    pub token_type: TokenType,           // Reuse existing enum
    pub position: usize,
}
```

### Template Types

```rust
/// A recognized command template
#[derive(Debug, Clone)]
pub struct Template {
    pub id: i64,
    pub pattern: String,                 // "docker run -p <PORT>:<PORT> <IMAGE>"
    pub base_command: String,
    pub placeholders: Vec<Placeholder>,
    pub frequency: u32,
}

#[derive(Debug, Clone)]
pub struct Placeholder {
    pub name: String,                    // PORT, IMAGE, PATH, etc.
    pub placeholder_type: PlaceholderType,
    pub position: usize,                 // Token position in template
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlaceholderType {
    Port,                                // 8080, 8080:8080
    Path,                                // File or directory
    Image,                               // Docker image
    Branch,                              // Git branch
    Url,                                 // URL or git remote
    Number,                              // Numeric value
    Quoted,                              // Quoted string
    Generic,                             // Any value
}

/// A filled template ready for insertion
#[derive(Debug, Clone)]
pub struct TemplateCompletion {
    pub template: Template,
    pub filled_values: HashMap<String, String>,
    pub preview: String,                 // Complete command preview
    pub confidence: f64,
}
```

### User Pattern Types

```rust
/// A user-defined suggestion pattern
#[derive(Debug, Clone)]
pub struct UserPattern {
    pub id: i64,
    pub pattern_type: UserPatternType,
    pub trigger: String,
    pub suggestion: String,
    pub description: Option<String>,
    pub priority: i32,
    pub enabled: bool,
    pub use_count: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UserPatternType {
    /// Simple alias expansion
    Alias,

    /// After command X, suggest Y
    Sequence,

    /// For files matching pattern, suggest command
    FileType,

    /// Custom trigger condition
    Trigger,
}

/// A user-defined alias
#[derive(Debug, Clone)]
pub struct UserAlias {
    pub id: i64,
    pub alias: String,
    pub expansion: String,
    pub description: Option<String>,
    pub enabled: bool,
    pub use_count: u32,
}
```

### Export/Import Types

```rust
/// Export format for pattern sharing
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PatternExport {
    pub version: String,                 // "1.0"
    pub exported_at: i64,
    pub machine_id: Option<String>,
    pub patterns: ExportedPatterns,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExportedPatterns {
    pub sequences: Vec<ExportedSequence>,
    pub pipe_chains: Vec<ExportedPipeChain>,
    pub flag_values: Vec<ExportedFlagValue>,
    pub templates: Vec<ExportedTemplate>,
    pub user_patterns: Vec<UserPattern>,
    pub user_aliases: Vec<UserAlias>,
}

pub struct ExportOptions {
    pub include_user_patterns: bool,
    pub include_learned_patterns: bool,
    pub min_frequency: u32,              // Only export if used N+ times
    pub anonymize_paths: bool,           // Remove machine-specific paths
}

pub struct ImportOptions {
    pub mode: ImportMode,
    pub conflict_resolution: ConflictResolution,
}

pub enum ImportMode {
    Merge,                               // Merge with existing
    Replace,                             // Replace all
    Append,                              // Add without merging
}

pub enum ConflictResolution {
    KeepExisting,
    UseImported,
    MergeFrequency,                      // Combine frequency counts (see below)
}

pub struct ImportStats {
    pub sequences_imported: usize,
    pub pipe_chains_imported: usize,
    pub templates_imported: usize,
    pub user_patterns_imported: usize,
    pub conflicts_resolved: usize,
    pub skipped: usize,
}
```

#### ConflictResolution::MergeFrequency Semantics

When `MergeFrequency` is selected, conflicting patterns are merged as follows:

**Merge Rules:**

| Field | Merge Strategy |
|-------|----------------|
| `frequency` | Sum: `existing + imported` |
| `last_seen` | Maximum: `max(existing, imported)` |
| `success_count` | Sum: `existing + imported` |
| `failure_count` | Sum: `existing + imported` (preserves exact success rate) |

**Pattern Equivalence:**

Two patterns are considered equivalent (conflict) when their **normalized canonical
form** matches. Normalization steps:

1. Trim leading/trailing whitespace
2. Collapse multiple spaces to single space
3. Case-sensitive comparison (commands are case-sensitive)
4. For sequences: match on `(context_token, context_position, base_command, next_token)`
5. For templates: match on `template_hash` (SHA-256 of normalized template string)

**Example:**

```
Machine A: git commit -m (frequency=100, success=95, last_seen=2026-02-01)
Machine B: git commit -m (frequency=50,  success=48, last_seen=2026-02-04)

After MergeFrequency:
Result:    git commit -m (frequency=150, success=143, last_seen=2026-02-04)
```

Note: Success rate is preserved exactly because both numerator (success_count)
and denominator (frequency) are summed: `(95+48)/(100+50) = 143/150 = 95.3%`
```

---

## Implementation Phases

### Phase 1: Core Infrastructure

**Goal**: Database schema, sync mechanism, basic token analysis.

**Files**:
- `src/intelligence/mod.rs`
- `src/intelligence/types.rs`
- `src/intelligence/error.rs`
- `src/intelligence/schema.rs`
- `src/intelligence/sync.rs`
- `src/intelligence/tokenizer.rs`

**Key struct**:

```rust
pub struct CommandIntelligence {
    conn: Connection,
    token_cache: HashMap<String, i64>,
    last_sync_id: i64,
    current_session: Option<SessionContext>,
    enabled: bool,
}

impl CommandIntelligence {
    pub fn new(conn: Connection) -> Result<Self, CIError>;
    pub fn sync(&mut self) -> Result<SyncStats, CIError>;
    pub fn analyze(&self, command: &str) -> Vec<AnalyzedToken>;
    fn get_or_create_token(&mut self, text: &str, token_type: TokenType) -> Result<i64>;
}
```

**Deliverables**:
- [ ] All tables created with proper indexes
- [ ] Schema versioning and migration support
- [ ] Incremental sync from reedline history
- [ ] Token analysis extending existing classification
- [ ] FTS5 virtual table and triggers

---

### Phase 2: Pattern Learning

**Goal**: Learn sequences, pipes, flags, n-grams from history.

**Files**:
- `src/intelligence/patterns/mod.rs`
- `src/intelligence/patterns/sequences.rs`
- `src/intelligence/patterns/pipes.rs`
- `src/intelligence/patterns/flags.rs`

**Key functions**:

```rust
impl CommandIntelligence {
    pub fn learn_command(&mut self, command: &str, exit_status: Option<i32>) -> Result<()>;

    fn learn_sequences(&mut self, tokens: &[AnalyzedToken], token_ids: &[i64]) -> Result<()>;
    fn learn_ngrams(&mut self, tokens: &[AnalyzedToken], token_ids: &[i64]) -> Result<()>;
    fn learn_pipe_chains(&mut self, tokens: &[AnalyzedToken], token_ids: &[i64]) -> Result<()>;
    fn learn_flag_values(&mut self, tokens: &[AnalyzedToken], token_ids: &[i64]) -> Result<()>;
}
```

**Transaction Handling:**

All sub-operations within `learn_command` must execute inside a single explicit
database transaction for atomic rollback on failure:

```rust
pub fn learn_command(&mut self, command: &str, exit_status: Option<i32>) -> Result<()> {
    self.conn.execute_batch("BEGIN TRANSACTION")?;

    let result = (|| {
        let tokens = self.analyze(command);
        let token_ids = self.get_or_create_tokens(&tokens)?;
        self.learn_sequences(&tokens, &token_ids)?;
        self.learn_ngrams(&tokens, &token_ids)?;
        self.learn_pipe_chains(&tokens, &token_ids)?;
        self.learn_flag_values(&tokens, &token_ids)?;
        Ok(())
    })();

    match result {
        Ok(()) => self.conn.execute_batch("COMMIT")?,
        Err(e) => {
            let _ = self.conn.execute_batch("ROLLBACK");
            return Err(e);
        }
    }
    Ok(())
}
```

**Async/Batch Processing (Optional Optimization):**

To reduce latency on the hot path (command submission), learning can be deferred:

1. **Immediate Mode** (default): Learn synchronously after command completion
2. **Batched Mode**: Queue commands and flush every N commands or T seconds

```rust
struct LearningBatcher {
    queue: Vec<(String, Option<i32>)>,
    max_batch_size: usize,          // Default: 50 commands
    flush_interval: Duration,        // Default: 30 seconds
    last_flush: Instant,
}

impl LearningBatcher {
    fn enqueue(&mut self, command: String, exit_status: Option<i32>) {
        self.queue.push((command, exit_status));
        if self.queue.len() >= self.max_batch_size ||
           self.last_flush.elapsed() > self.flush_interval {
            self.flush();
        }
    }

    fn flush(&mut self) {
        // Process all queued commands in a single transaction
        // Retry on transient errors (SQLITE_BUSY)
    }
}
```

**Database Pragmas (enable in schema creation):**

```sql
PRAGMA journal_mode = WAL;           -- Better concurrent write performance
PRAGMA synchronous = NORMAL;         -- Balance durability vs speed
PRAGMA busy_timeout = 5000;          -- Wait 5s on locks before failing
```

**Deliverables**:
- [x] Token sequences stored with position context
- [x] 2-gram and 3-gram patterns extracted
- [x] Pipe chains with full post-pipe context
- [x] Flag-value associations with type detection
- [x] Success counts updated for exit_status == 0
- [x] All sub-operations wrapped in single transaction
- [ ] Optional: Batched/async learning mode

---

### Phase 3: Session & Transition Tracking

**Goal**: Track commands within sessions, learn transitions.

**File**: `src/intelligence/sessions.rs`

**Key functions**:

```rust
impl CommandIntelligence {
    /// Start or resume a session
    pub fn start_session(&mut self, session_id: &str) -> Result<()>;

    /// End current session
    pub fn end_session(&mut self) -> Result<()>;

    /// Record command in session and learn transitions
    pub fn track_session_command(&mut self, command: &str) -> Result<()>;

    /// Get likely next commands based on last command
    pub fn suggest_next_in_session(&self, last_command: &str) -> Vec<Suggestion>;
}
```

**Deliverables**:
- [ ] Sessions tracked with unique IDs
- [ ] Commands recorded with sequence numbers
- [ ] Transitions learned (A → B patterns)
- [ ] Time deltas between commands tracked
- [ ] "What's next" suggestions based on session history

---

### Phase 4: Template Recognition

**Goal**: Extract and match command templates with placeholders.

**File**: `src/intelligence/templates.rs`

**Key functions**:

```rust
impl CommandIntelligence {
    /// Extract template from command
    pub fn extract_template(&mut self, command: &str) -> Option<Template>;

    /// Learn template values from command
    pub fn learn_template_values(&mut self, command: &str) -> Result<()>;

    /// Get template completions for context
    pub fn suggest_templates(&self, context: &SuggestionContext) -> Vec<TemplateCompletion>;

    /// Detect placeholder type
    fn detect_placeholder_type(&self, value: &str, position: usize) -> PlaceholderType;
}
```

**Template extraction rules**:
- Port patterns: `\d+:\d+` or `\d{2,5}` → `<PORT>`
- Paths: Contains `/` or starts with `.` → `<PATH>`
- Docker images: `[\w-]+/[\w-]+:?[\w.-]*` → `<IMAGE>`
- Git branches: After `checkout`, `switch`, `branch` → `<BRANCH>`
- Quoted strings: `"..."` or `'...'` → `<QUOTED>`
- URLs: Contains `://` → `<URL>`
- Numbers: Pure digits → `<NUMBER>`

**Deliverables**:
- [ ] Templates extracted from high-frequency commands
- [ ] Placeholders detected and typed
- [ ] Template values stored with frequency
- [ ] Template completion suggestions work
- [ ] Preview shows filled template

---

### Phase 5: Failure Learning

**Goal**: Track success/failure and prefer working variants.

**File**: `src/intelligence/variants.rs`

**Key functions**:

```rust
impl CommandIntelligence {
    /// Record execution result
    pub fn record_execution(&mut self, command: &str, exit_status: i32) -> Result<()>;

    /// Get success rate for command pattern
    pub fn get_success_rate(&self, command: &str) -> Option<f64>;

    /// Get successful variants of a pattern
    pub fn get_successful_variants(&self, pattern: &str) -> Vec<CommandVariant>;

    /// Canonicalize command to pattern
    fn canonicalize(&self, command: &str) -> String;
}
```

**Canonicalization**:
- Normalize whitespace
- Replace specific values with type markers
- Keep command structure

Example: `docker run -p 8080:8080 nginx` → `docker_run_-p_<PORT>_<IMAGE>`

**Deliverables**:
- [ ] Success/failure counts tracked
- [ ] Success rate computed automatically
- [ ] Failed variants penalized in suggestions
- [ ] Successful variants boosted

---

### Phase 6: FTS5 Fuzzy Search

**Goal**: Typo tolerance and partial matching.

**File**: `src/intelligence/fuzzy.rs`

**Key functions**:

```rust
impl CommandIntelligence {
    /// Fuzzy search for commands
    pub fn fuzzy_search(&self, query: &str, limit: usize) -> Vec<FuzzyMatch>;

    /// Get suggestions with fuzzy fallback
    fn suggest_fuzzy(&self, context: &SuggestionContext) -> Vec<Suggestion>;
}

pub struct FuzzyMatch {
    pub command: String,
    pub bm25_score: f64,
    pub matched_terms: Vec<String>,
}
```

**Deliverables**:
- [ ] FTS5 queries return ranked results
- [ ] Typo correction (e.g., "dockr" → "docker")
- [ ] Partial matching works
- [ ] BM25 scoring integrated into overall ranking

---

### Phase 7: Suggestion Engine

**Goal**: Unified suggestion generation with ranking.

**Files**:
- `src/intelligence/suggest.rs`
- `src/intelligence/scoring.rs`

**Key functions**:

```rust
impl CommandIntelligence {
    /// Main suggestion entry point
    pub fn suggest(&self, context: &SuggestionContext, limit: usize) -> Vec<Suggestion>;

    /// Aggregate from all sources
    fn gather_suggestions(&self, context: &SuggestionContext) -> Vec<Suggestion>;

    /// Deduplicate and rank
    fn rank_suggestions(&self, suggestions: Vec<Suggestion>) -> Vec<Suggestion>;

    /// Compute frecency score
    fn compute_score(&self, frequency: u32, last_seen: i64, success_rate: Option<f64>) -> f64;
}
```

**Suggestion priority order**:
1. User patterns (highest priority if matched)
2. Session transitions (if recent command matches)
3. Learned sequences (exact context match)
4. Learned pipe chains (if after pipe)
5. Learned flag values (if after flag)
6. Templates (if partial matches template)
7. N-gram patterns
8. FTS5 fuzzy search (fallback for typos)
9. Static knowledge (fallback)

**Scoring formula**:

```
score = base_score * context_bonus * success_bonus * source_bonus

base_score = ln(1 + frequency) * recency_weight
recency_weight = min(1.0, 1.0 / (1.0 + days_since_use * 0.1))
success_bonus = 0.5 + (success_rate * 0.5)
context_bonus = 1.5 (exact) | 1.2 (base_cmd) | 1.0 (generic)
source_bonus = 2.0 (user) | 1.5 (session) | 1.2 (learned) | 1.0 (static)
```

**Edge Case Handling** (implemented in `compute_score`):

| Condition | Handling |
|-----------|----------|
| `frequency = 0` | `ln(1+0) = 0`, safe - results in `base_score = 0` |
| `days_since_use < 0` (future timestamp) | Clamp to 0: `max(0, (now - last_seen) / 86400)` |
| `recency_weight > 1.0` | Cap at 1.0 to prevent score inflation |
| `success_rate = None` | Default to 0.75 (neutral bonus) to avoid NaN |
| `success_rate` out of range | Clamp to [0.0, 1.0] |

**Deduplication Strategy** (implemented in `rank_suggestions`):

Duplicates are detected by **exact text match** (case-sensitive). When collapsing:

1. Keep the highest-scoring `Suggestion` as primary
2. Aggregate metadata from all duplicates:
   - `frequency`: sum of all frequencies
   - `last_seen`: maximum (most recent) timestamp
3. Record all contributing sources for UI display
4. Secondary (lower-scoring) sources are preserved internally

**Deliverables**:
- [x] All sources aggregated
- [x] Deduplication with best-score retention and metadata aggregation
- [x] Frecency ranking implemented with edge-case handling
- [x] Success rate influences ranking (with default for missing values)
- [x] Source bonuses applied
- [ ] Performance: < 50ms

---

### Phase 8: User Patterns

**Goal**: User-defined aliases and custom suggestions.

**File**: `src/intelligence/user_patterns.rs`

**Key functions**:

```rust
impl CommandIntelligence {
    // Pattern management
    pub fn add_user_pattern(&mut self, pattern: UserPattern) -> Result<i64>;
    pub fn update_user_pattern(&mut self, id: i64, pattern: UserPattern) -> Result<()>;
    pub fn remove_user_pattern(&mut self, id: i64) -> Result<()>;
    pub fn list_user_patterns(&self, pattern_type: Option<UserPatternType>) -> Vec<UserPattern>;
    pub fn enable_user_pattern(&mut self, id: i64, enabled: bool) -> Result<()>;

    // Alias management
    pub fn add_alias(&mut self, alias: &str, expansion: &str, description: Option<&str>) -> Result<i64>;
    pub fn remove_alias(&mut self, alias: &str) -> Result<()>;
    pub fn list_aliases(&self) -> Vec<UserAlias>;
    pub fn expand_alias(&self, text: &str) -> Option<String>;

    // Query user patterns for suggestions
    fn suggest_from_user_patterns(&self, context: &SuggestionContext) -> Vec<Suggestion>;
}
```

**Pattern types**:
- **Alias**: `deploy` → `git push && ssh prod 'cd /app && docker-compose up -d'`
- **Sequence**: After `make test` → suggest `make build`
- **FileType**: For `*.rs` files → suggest `cargo run`
- **Trigger**: Custom conditions

**Deliverables**:
- [ ] CRUD operations for patterns
- [ ] CRUD operations for aliases
- [ ] Patterns integrated into suggestion flow
- [ ] User patterns have highest priority
- [ ] Use counts tracked

---

### Phase 9: Export/Import

**Goal**: Share patterns across machines.

**File**: `src/intelligence/export.rs`

**Key functions**:

```rust
impl CommandIntelligence {
    /// Export patterns to JSON
    pub fn export(&self, options: ExportOptions) -> Result<String, CIError>;

    /// Import patterns from JSON
    pub fn import(&mut self, json: &str, options: ImportOptions) -> Result<ImportStats, CIError>;

    /// Validate import JSON
    fn validate_import(&self, json: &str) -> Result<PatternExport, CIError>;
}
```

**Export format**: See `PatternExport` type definition above.

**Deliverables**:
- [ ] Export generates valid JSON
- [ ] Frequency threshold filtering
- [ ] Path anonymization option
- [ ] Import validates schema
- [ ] Merge mode combines frequencies
- [ ] Replace mode overwrites
- [ ] Import statistics returned

---

### Phase 10: Integration

**Goal**: Wire into existing UI components.

**Files to modify**:
- `src/history_store.rs`
- `src/chrome/command_edit.rs`
- `src/chrome/history_browser.rs`
- `src/chrome/file_browser.rs`

**Integration**:

```rust
// history_store.rs
impl HistoryStore {
    /// Lazily initializes and returns the CommandIntelligence instance.
    /// Returns None if initialization fails (graceful degradation).
    pub fn intelligence(&mut self) -> Option<&mut CommandIntelligence>;

    /// Delegates to intelligence().suggest(), returning empty on None.
    pub fn intelligent_suggest(
        &mut self,
        context: &SuggestionContext,
        limit: usize,
    ) -> Vec<Suggestion>;

    /// Delegates to intelligence().learn_command(), logging errors.
    pub fn on_command_complete(&mut self, command: &str, exit_status: i32);

    pub fn start_session(&mut self, session_id: &str);
    pub fn end_session(&mut self);
}

// command_edit.rs
impl CommandEditState {
    pub fn build_context(&self, cwd: Option<&Path>, session: Option<&SessionContext>) -> SuggestionContext;
}
```

**Lazy Initialization:**

`CommandIntelligence` is initialized on first access via `intelligence()`:

```rust
fn intelligence(&mut self) -> Option<&mut CommandIntelligence> {
    if self.ci_initialized {
        return self.ci.as_mut();
    }
    self.ci_initialized = true;

    match CommandIntelligence::from_path(&self.db_path) {
        Ok(ci) => {
            tracing::info!("CommandIntelligence initialized");
            self.ci = Some(ci);
            self.ci.as_mut()
        }
        Err(e) => {
            tracing::warn!("Failed to initialize CommandIntelligence: {}", e);
            None  // Graceful degradation - intelligence disabled
        }
    }
}
```

**Error Handling:**

All intelligence operations use `log::warn` and return graceful defaults:

| Operation | Error Behavior |
|-----------|----------------|
| `intelligence()` | Returns `None`, logs warning |
| `intelligent_suggest()` | Returns empty `Vec<Suggestion>` |
| `on_command_complete()` | Logs warning, continues silently |
| `start_session()` | Logs warning, continues silently |
| `end_session()` | Logs warning, continues silently |
| `sync()` | Logs warning, skips sync cycle |
| Schema creation failure | Returns `CIError::Schema`, disables intelligence |

**Session Lifecycle:**

Session IDs should be generated deterministically or via UUID on `start_session()`:

```rust
// On terminal open (in App::new or similar)
let session_id = uuid::Uuid::new_v4().to_string();
history_store.start_session(&session_id);

// On terminal close (in App::cleanup or Drop)
history_store.end_session();
```

Session tracking ties to `CommandIntelligence` for:
- Transition suggestions ("what's next" based on previous command)
- Session-scoped command sequences
- Activity tracking for timeout/cleanup

**Deliverables**:
- [x] Intelligence initialized lazily via `intelligence()` accessor
- [x] Graceful fallback on errors (warn + return None/empty)
- [ ] Session tracking on terminal open/close
- [ ] Intelligent suggestions in history browser
- [ ] Intelligent suggestions in file browser
- [ ] Immediate learning on command complete

---

### Phase 11: Optimization

**Goal**: Performance and polish.

**Optimizations**:
- Prepared statement caching
- LRU cache for suggestions (with TTL)
- Batch updates in transactions
- Background sync
- Cache invalidation on new commands

**Cache Management (ci_suggestion_cache):**

See [Caching Tables](#caching-tables) section for full schema. Key implementation details:

1. **Periodic Cleanup Job** (run on startup and every 60 seconds):
   ```sql
   DELETE FROM ci_suggestion_cache WHERE expires_at < strftime('%s', 'now');
   ```

2. **Explicit Invalidation Points**:
   - After `learn_command()`: invalidate cache keys matching base command
   - After user pattern CRUD: clear entire cache
   - After `import()`: clear entire cache

3. **Max Size Policy** (10,000 entries):
   - Before inserting new entry, check count
   - If exceeding limit, delete oldest entries by `computed_at`

4. **TTL Computation** (standardized across all code paths):
   ```rust
   const CACHE_TTL_SECONDS: i64 = 300; // 5 minutes
   let expires_at = chrono::Utc::now().timestamp() + CACHE_TTL_SECONDS;
   ```

5. **LRU Eviction** (when size threshold reached):
   ```sql
   DELETE FROM ci_suggestion_cache
   WHERE cache_key IN (
       SELECT cache_key FROM ci_suggestion_cache
       ORDER BY computed_at ASC
       LIMIT (SELECT COUNT(*) - 10000 FROM ci_suggestion_cache)
   );
   ```

**Polish**:
- Source indicators in UI
- Confidence/score display option
- Settings for enable/disable
- Settings for export location

**Deliverables**:
- [ ] Suggestions consistently < 50ms
- [ ] Sync non-blocking
- [ ] Source visibility in UI
- [ ] Configurable via settings
- [x] Cache TTL and eviction documented
- [ ] Cache invalidation implemented at all specified points

---

## Performance Targets

| Operation | Target | Notes |
|-----------|--------|-------|
| Schema creation | < 200ms | One-time, includes FTS5 |
| Incremental sync (100 cmds) | < 500ms | Background |
| Pattern learning (per cmd) | < 30ms | On completion |
| Suggestion query | < 50ms | Interactive |
| Template matching | < 20ms | Part of suggestions |
| FTS5 search | < 30ms | Fallback only |
| Export (10K patterns) | < 1s | File I/O |
| Import (10K patterns) | < 2s | With merge |
| Memory overhead | < 20MB | Bounded caches |

---

## Testing Strategy

### Unit Tests

- **Schema**: Tables created, migrations work
- **Tokenizer**: Classification accuracy
- **Patterns**: Extraction correctness
- **Sessions**: Transition tracking
- **Templates**: Placeholder detection
- **Variants**: Canonicalization consistency
- **Scoring**: Formula correctness
- **User patterns**: CRUD operations
- **Export/Import**: Round-trip fidelity

### Integration Tests

- **Sync**: Processes reedline correctly
- **Suggestions**: Relevant results for contexts
- **Fallback**: Graceful degradation
- **Performance**: Latency under load

### Benchmarks

- Suggestion latency with 10K/50K/100K history
- FTS5 query performance
- Export/import with large datasets
- Memory usage under load

---

## Reedline Compatibility

| Guarantee | How |
|-----------|-----|
| Never write to `history` | Only SELECT from reedline tables |
| No schema conflicts | All tables use `ci_` prefix |
| Graceful fallback | Errors return static suggestions |
| Independent versions | Schema versioned in `ci_schema_version` |

---

## Success Criteria

- [ ] All tables created and indexed
- [ ] Sync processes history correctly
- [ ] Pattern learning extracts sequences, pipes, flags
- [ ] Session transitions tracked
- [ ] Templates recognized and suggested
- [ ] Failure learning prefers successful variants
- [ ] FTS5 handles typos
- [ ] User patterns with highest priority
- [ ] Export/import works across machines
- [ ] Suggestions < 50ms
- [ ] UI shows suggestion sources
- [ ] Comprehensive test coverage
- [ ] All features configurable

---

## Example Scenarios

### Scenario 1: Pipe Completion

**User types**: `cat /var/log/syslog |`

**System**: AfterPipe position, base command `cat`, file `.log`

**Suggestions**:
1. `grep` (learned: 45x after cat *.log)
2. `awk` (learned: 12x)
3. `sort` (static knowledge)

---

### Scenario 2: Session-Aware "Next"

**Session so far**:
1. `cd ~/projects/myapp`
2. `git pull`

**User**: Empty prompt

**Suggestions** (session transitions):
1. `cargo build` (transition: after git pull in this cwd)
2. `cargo test` (transition: second most common)

---

### Scenario 3: Template Completion

**User types**: `docker run -p`

**System recognizes template**: `docker run -p <PORT>:<PORT> <IMAGE>`

**Suggestions**:
1. `8080:8080 nginx:latest` (most common fill)
2. `3000:3000 node:18` (second most common)
3. [template preview mode]

---

### Scenario 4: Typo Recovery

**User types**: `dockr ps`

**FTS5 fuzzy match**: "docker ps" (edit distance 1)

**Suggestion**: `docker` (with "did you mean?" indicator)

---

### Scenario 5: User Pattern Override

**User-defined**: Alias `deploy` → `git push && ssh prod 'docker-compose up -d'`

**User types**: `dep`

**Suggestions**:
1. `deploy` → expands to full command (UserAlias, priority)
2. `deps` (if exists in history)

---

## File Size Estimates

```
src/intelligence/
├── mod.rs                    # 100 lines
├── types.rs                  # 250 lines
├── error.rs                  # 50 lines
├── schema.rs                 # 200 lines
├── sync.rs                   # 150 lines
├── tokenizer.rs              # 150 lines
├── patterns/
│   ├── mod.rs                # 100 lines
│   ├── sequences.rs          # 150 lines
│   ├── pipes.rs              # 120 lines
│   └── flags.rs              # 100 lines
├── sessions.rs               # 200 lines
├── templates.rs              # 250 lines
├── variants.rs               # 150 lines
├── fuzzy.rs                  # 100 lines
├── suggest.rs                # 300 lines
├── scoring.rs                # 100 lines
├── user_patterns.rs          # 200 lines
└── export.rs                 # 200 lines

Total new code: ~2,570 lines

Modifications to existing:
├── history_store.rs          # +80 lines
├── chrome/command_edit.rs    # +40 lines
├── chrome/history_browser.rs # +30 lines
└── chrome/file_browser.rs    # +30 lines

Total modifications: ~180 lines
```

---

*Last updated: 2026-02-04*
