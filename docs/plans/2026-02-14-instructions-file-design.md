# Design: File-backed `instructions_file` for Claude Config

## Problem

The `claude.instructions` config field requires inline text in the YAML config file. For large instruction sets this makes the config file unwieldy and hard to maintain.

## Solution

Add an `instructions_file` field to `ClaudeConfig` that references an external file containing instructions.

## Config Schema

```yaml
claude:
  model: sonnet
  instructions_file: "./claude-instructions.md"  # path to file (relative to config file)
  instructions: "Additional inline overrides"     # existing field, unchanged
```

## Behavior

### Path Resolution
- `instructions_file` resolves relative to the config file's parent directory.
- Absolute paths are also supported.

### Combination (both set)
When both `instructions_file` and `instructions` are provided:
1. File content comes first.
2. Inline `instructions` appended after, separated by a newline.

Either field works independently when the other is absent.

### Environment Variable Support
- Existing: `CLAUDE_INSTRUCTIONS` overrides `instructions` (inline text).
- New: `CLAUDE_INSTRUCTIONS_FILE` overrides `instructions_file` (file path, resolves relative to CWD).

### Error Handling
- File not found or unreadable: hard error at config load time with a clear message.
- Empty file: treated as no instructions from file (no error).

## Implementation

### New method on `ClaudeConfig`

`resolve_instructions(&self, config_dir: &Path) -> Result<Option<String>>` that:
1. Reads file content if `instructions_file` is set.
2. Concatenates file content + inline instructions.
3. Returns the combined string.

### Files Changed
- `src/config.rs`: Add `instructions_file` field, env override, `resolve_instructions()` method, tests.
- `src/runner.rs` or `src/watcher.rs`: Call `resolve_instructions()` instead of reading `instructions` directly.
- `claudear.example.yaml`: Document the new field.
