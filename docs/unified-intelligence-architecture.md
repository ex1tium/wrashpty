# Unified Command Intelligence Architecture

## Overview

This document describes a cohesive, learning-first architecture for command intelligence that:

1. **Learns everything dynamically** - no hardcoded command knowledge
2. **Is transparent to users** - they just see smart suggestions
3. **Has clear subsystem boundaries** - each component has one job
4. **Uses position-aware token learning** - naturally handles subcommands, arguments, flags

---

## Current State Analysis

### What Works Well

| Component | Strength |
|-----------|----------|
| `ci_sequences` | Already tracks `context_position` and `base_command_id` |
| `ci_tokens` | Good vocabulary table with frequency/recency |
| `ci_commands` | Stores full commands with exit status |
| `variants` | Success/failure tracking per command pattern |
| `sessions` | Command-to-command workflow learning |

### Current Problems

1. **Static Knowledge Conflict**: `command_knowledge.rs` hardcodes git/docker/cargo subcommands, but this overlaps with learned patterns and stops at position 2.

2. **Granularity Mismatch**: Some sources return tokens (`commit`), others return full commands (`git push origin main`), causing the `git remote add` bug.

3. **No Hierarchical Learning**: The system doesn't explicitly learn that `git` has subcommand `remote`, which has nested command `add`.

4. **Position Gaps**: After position 2, static knowledge returns nothing, forcing reliance on learned patterns that may not exist yet.

---

## Unified Architecture

### Core Principle: Learn the Command Grammar

Instead of separate systems, we model commands as a **learned grammar**:

```
Command ::= BaseCommand [Subcommand [NestedCommand]] [Flags] [Arguments]*
```

The system learns:
- What tokens appear at position 0 (base commands)
- What tokens follow each base command at position 1 (subcommands)
- What tokens follow each (base, subcommand) pair at position 2 (nested/flags/args)
- And so on...

### New Schema: Command Hierarchy Table

```sql
CREATE TABLE ci_command_hierarchy (
    id INTEGER PRIMARY KEY,

    -- The token that appears at this position
    token_id INTEGER NOT NULL,

    -- Position in command (0 = base, 1 = subcommand, etc.)
    position INTEGER NOT NULL,

    -- Parent hierarchy node ID (NULL for position 0)
    -- This captures the full learned prefix, not just the immediate token text.
    parent_node_id INTEGER,

    -- Base command ID (for fast filtering)
    base_command_id INTEGER,

    -- Statistics
    frequency INTEGER DEFAULT 1,
    success_count INTEGER DEFAULT 0,
    last_seen INTEGER NOT NULL,

    -- Semantic classification learned over time
    role TEXT,  -- 'subcommand', 'flag', 'argument', 'value'

    UNIQUE(token_id, position, parent_node_id, base_command_id),
    FOREIGN KEY (token_id) REFERENCES ci_tokens(id),
    FOREIGN KEY (parent_node_id) REFERENCES ci_command_hierarchy(id),
    FOREIGN KEY (base_command_id) REFERENCES ci_tokens(id)
);

CREATE INDEX idx_hierarchy_parent ON ci_command_hierarchy(parent_node_id, position);
CREATE INDEX idx_hierarchy_base ON ci_command_hierarchy(base_command_id, position);
```

### How Learning Works

When user executes: `git remote add origin https://github.com/user/repo.git`

1. **Tokenize**: `[git, remote, add, origin, https://...]`
2. **Learn hierarchy**:
   - Position 0: `git` (base command, parent=NULL)
   - Position 1: `remote` (parent=git, base=git)
   - Position 2: `add` (parent=remote, base=git)
   - Position 3: `origin` (parent=add, base=git)
   - Position 4: `https://...` (parent=origin, base=git, role=URL)
3. **Update frequencies** if patterns already exist

### Unified Suggestion Query

```rust
/// Single entry point for all suggestions
pub fn suggest_at_position(
    conn: &Connection,
    context: &SuggestionContext,
) -> Vec<Suggestion> {
    let position = context.preceding_tokens.len();

    match position {
        0 => suggest_base_commands(conn),
        _ => {
            let parent_token = context.preceding_tokens.last().map(|t| &t.text);
            let base_command = context.preceding_tokens.first().map(|t| &t.text);

            suggest_children(conn, position, parent_token, base_command)
        }
    }
}

fn suggest_children(
    conn: &Connection,
    position: usize,
    parent_token: Option<&str>,
    base_command: Option<&str>,
) -> Vec<Suggestion> {
    // Query hierarchy table for tokens at this position
    // with the given parent and base command
    let query = "
        SELECT t.text, h.frequency, h.success_count, h.last_seen, h.role
        FROM ci_command_hierarchy h
        JOIN ci_tokens t ON t.id = h.token_id
        WHERE h.position = ?1
          AND h.parent_node_id = ?2
          AND h.base_command_id = (SELECT id FROM ci_tokens WHERE text = ?3)
        ORDER BY h.frequency DESC
        LIMIT 20
    ";

    // Execute and map to suggestions...
}
```

---

## Subsystem Responsibilities (Revised)

### 1. Learning Subsystem (`patterns/`)

**Single Responsibility**: Ingest executed commands and update the knowledge graph.

```
┌─────────────────────────────────────────────────────────────┐
│                    LEARNING PIPELINE                         │
│                                                             │
│  Command Executed                                           │
│       │                                                     │
│       ▼                                                     │
│  ┌─────────────┐                                           │
│  │  Tokenize   │                                           │
│  └──────┬──────┘                                           │
│         │                                                   │
│         ▼                                                   │
│  ┌─────────────────────────────────────────────────────┐   │
│  │              UPDATE LEARNED DATA                     │   │
│  │                                                      │   │
│  │  • ci_tokens (vocabulary)                           │   │
│  │  • ci_command_hierarchy (structure)                 │   │
│  │  • ci_sequences (n-grams, kept for compatibility)   │   │
│  │  • ci_command_variants (success/failure)            │   │
│  │  • ci_templates (parameterized patterns)            │   │
│  │  • ci_transitions (session workflows)               │   │
│  └─────────────────────────────────────────────────────┘   │
│                                                             │
└─────────────────────────────────────────────────────────────┘
```

### 2. Suggestion Subsystem (`suggest.rs`)

**Single Responsibility**: Query learned data and rank suggestions.

```
┌─────────────────────────────────────────────────────────────┐
│                   SUGGESTION PIPELINE                        │
│                                                             │
│  Cursor Position + Context                                  │
│       │                                                     │
│       ▼                                                     │
│  ┌─────────────────────────────────────────────────────┐   │
│  │         POSITION-AWARE QUERY (PRIMARY)               │   │
│  │                                                      │   │
│  │  Query ci_command_hierarchy for tokens at position  │   │
│  │  N with parent=preceding_token, base=first_token    │   │
│  └──────────────────────┬──────────────────────────────┘   │
│                         │                                   │
│                         ▼                                   │
│  ┌─────────────────────────────────────────────────────┐   │
│  │           SUPPLEMENTARY SOURCES                      │   │
│  │                                                      │   │
│  │  • Flag values (if position is after a flag)        │   │
│  │  • Template values (if matching template context)   │   │
│  │  • N-grams (for multi-token pattern matching)       │   │
│  └──────────────────────┬──────────────────────────────┘   │
│                         │                                   │
│                         ▼                                   │
│  ┌─────────────────────────────────────────────────────┐   │
│  │              SCORING & RANKING                       │   │
│  │                                                      │   │
│  │  score = frequency × recency × success_rate         │   │
│  │                                                      │   │
│  │  Deduplicate, sort, limit                           │   │
│  └─────────────────────────────────────────────────────┘   │
│                                                             │
└─────────────────────────────────────────────────────────────┘
```

### 3. Session Subsystem (`sessions.rs`)

**Single Responsibility**: Track command-to-command workflows.

This remains separate because it operates at a different granularity:
- Token suggestions: "What token comes next in THIS command?"
- Session suggestions: "What COMMAND should I run after this one?"

```rust
// Session suggestions shown in a DIFFERENT UI area
// Not mixed with token completion
pub fn suggest_next_command(conn: &Connection, last_command: &str) -> Vec<CommandSuggestion> {
    // Returns full commands, not tokens
    // Displayed in a "workflow hint" area, not inline completion
}
```

### 4. Bootstrap Subsystem (NEW)

**Single Responsibility**: Seed initial knowledge for cold-start.

```rust
pub fn bootstrap_if_empty(conn: &Connection) -> Result<(), CIError> {
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM ci_command_hierarchy",
        [],
        |row| row.get(0)
    )?;

    if count == 0 {
        // Import from static knowledge ONCE
        seed_from_static_knowledge(conn)?;
    }

    Ok(())
}
```

After bootstrap, static knowledge is never consulted again. The learned data takes over completely.

---

## Deprecation Plan

### Phase 1: Add Hierarchy Table (Non-Breaking)

1. Add `ci_command_hierarchy` table
2. Modify `learn_command()` to populate hierarchy alongside sequences
3. Both systems run in parallel

### Phase 2: Switch Primary Source

1. Modify `suggest.rs` to query hierarchy first
2. Fall back to sequences only if hierarchy returns empty
3. Remove `token_mode` flag (no longer needed)

### Phase 3: Remove Static Knowledge

1. Move `command_knowledge.rs` to bootstrap-only
2. Run bootstrap on first launch
3. Delete static knowledge from suggestion pipeline
4. Remove `suggest_static()` function

---

## UI/UX: Transparent to User

The user sees:

```
┌──────────────────────────────────────────────────────────────────┐
│ $ git remote add ▌                                               │
│                                                                  │
│   ┌─ Token Suggestions ─────────────────────────────────────┐   │
│   │  origin          (used 47 times, 100% success)          │   │
│   │  upstream        (used 12 times, 100% success)          │   │
│   │  backup          (used 3 times, 100% success)           │   │
│   └─────────────────────────────────────────────────────────┘   │
│                                                                  │
│   ┌─ Workflow Hint ─────────────────────────────────────────┐   │
│   │  After 'git remote add', you usually run:              │   │
│   │    git fetch origin                                    │   │
│   │    git push -u origin main                             │   │
│   └─────────────────────────────────────────────────────────┘   │
│                                                                  │
└──────────────────────────────────────────────────────────────────┘
```

**Key UX points:**

1. **Token suggestions** are always single tokens (inline completion)
2. **Workflow hints** are full commands (separate UI area, optional)
3. **Templates** only appear when user explicitly requests them (e.g., Tab-Tab)
4. **No mixing** - token and command suggestions never appear in the same list

---

## Handling New Commands

When user runs a command the system hasn't seen:

```
$ mycli subcommand --flag value
```

**Learning:**
1. `mycli` added to position 0 tokens
2. `subcommand` added with parent=mycli at position 1
3. `--flag` added with parent=subcommand at position 2
4. `value` added with parent=--flag at position 3

**Next time:**
```
$ mycli ▌
  → subcommand (learned)
```

No patching required. The system learns any command structure automatically.

---

## Edge Case Resolution

### The `git remote add` Bug

**Before (current architecture):**
- Position 3: `suggest_arguments()` queries sequences
- Templates inject `git remote add <URL>` (full command)
- Static knowledge returns empty (only handles positions 0-2)
- Mixed granularity in results

**After (unified architecture):**
- Position 3: Query `ci_command_hierarchy` where parent=`add`, base=`git`, position=3
- Returns: `origin`, `upstream`, etc. (single tokens)
- Templates NOT queried for token completion
- Templates ONLY used when building workflow hints

### Cold Start (New Installation)

**Before:** Static knowledge always consulted, creates priority conflicts.

**After:**
1. Bootstrap runs once, seeding hierarchy from static data
2. All subsequent queries hit learned data only
3. As user runs commands, learned data supersedes bootstrap data
4. After ~100 commands, bootstrap data is effectively diluted

---

## Implementation Checklist

### Schema Changes (schema.rs)
- [ ] Add `ci_command_hierarchy` table
- [ ] Add indexes for parent/position/base queries
- [ ] Increment schema version

### Learning Changes (patterns/)
- [ ] Add `learn_hierarchy()` function
- [ ] Call from `learn_command()` after tokenization
- [ ] Classify token roles based on position and patterns

### Suggestion Changes (suggest.rs)
- [ ] Add `suggest_from_hierarchy()` function
- [ ] Make it the primary source for position > 0
- [ ] Remove `token_mode` flag
- [ ] Separate token suggestions from command predictions

### Bootstrap (NEW: bootstrap.rs)
- [ ] Create `bootstrap_if_empty()`
- [ ] Convert `command_knowledge.rs` data to hierarchy inserts
- [ ] Call on first CommandIntelligence initialization

### Cleanup
- [ ] Remove `suggest_static()` from suggestion pipeline
- [ ] Remove `token_mode` from `SuggestionContext`
- [ ] Archive `command_knowledge.rs` to `bootstrap/seed_data.rs`

---

## Scoring Formula (Unified)

All suggestions use the same scoring:

```rust
fn compute_score(
    frequency: u32,
    last_seen: i64,
    success_rate: Option<f64>,
) -> f64 {
    let now = chrono::Utc::now().timestamp();
    let age_days = (now - last_seen) as f64 / 86400.0;

    // Frequency: logarithmic to prevent runaway
    let freq_score = (1.0 + frequency as f64).ln();

    // Recency: exponential decay, half-life of 7 days
    let recency_score = (-age_days / 7.0).exp();

    // Success: boost successful commands, penalize failures
    let success_score = match success_rate {
        Some(rate) if rate >= 0.8 => 1.2,  // Boost
        Some(rate) if rate < 0.3 => 0.5,   // Penalize
        _ => 1.0,
    };

    freq_score * recency_score * success_score
}
```

No source bonuses. All suggestions compete on equal footing based on learned behavior.

---

## Conclusion

This architecture:

1. **Eliminates edge cases** by design - all suggestions come from one learned source
2. **Learns any command** without code changes
3. **Separates concerns clearly**: tokens vs commands, learning vs querying
4. **Provides transparent UX**: users see smart suggestions without knowing how

The key insight: instead of patching multiple overlapping systems, we unify around a single learned hierarchy that naturally models command structure.
