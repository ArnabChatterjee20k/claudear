# Instructions File Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Allow `claude.instructions` config to reference an external file via a new `instructions_file` field, so large instruction sets don't bloat the YAML config.

**Architecture:** Add `instructions_file: Option<String>` to `ClaudeConfig`. During `Config::load`, after env overrides, resolve the file path relative to the config file's directory, read its content, and merge it into the `instructions` field (file first, then inline appended). Downstream code (runner, watcher) remains unchanged.

**Tech Stack:** Rust, serde_yaml, std::fs, std::path

---

### Task 1: Add `instructions_file` field to `ClaudeConfig`

**Files:**
- Modify: `src/config.rs:16-38` (ClaudeConfig struct and Default impl)

**Step 1: Add the field to the struct**

In `ClaudeConfig` (line 18-28), add `instructions_file` after `instructions`:

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ClaudeConfig {
    /// Model to use (e.g., sonnet, opus, haiku, or full model ID).
    pub model: Option<String>,
    /// Custom instructions appended to Claude's system prompt.
    pub instructions: Option<String>,
    /// Path to a file containing custom instructions.
    /// Resolved relative to the config file directory. If both this and
    /// `instructions` are set, file content comes first, then inline appended.
    pub instructions_file: Option<String>,
    /// Tool permissions granted without prompting (--allowedTools).
    #[serde(default)]
    pub permissions: Vec<String>,
    /// Skip all permission prompts (default: true for backwards compat).
    pub skip_permissions: bool,
}
```

**Step 2: Update the Default impl**

In `Default for ClaudeConfig` (line 30-38), add `instructions_file: None`:

```rust
impl Default for ClaudeConfig {
    fn default() -> Self {
        Self {
            model: None,
            instructions: None,
            instructions_file: None,
            permissions: Vec::new(),
            skip_permissions: true,
        }
    }
}
```

**Step 3: Run `cargo check`**

Run: `cargo check 2>&1`
Expected: compiles cleanly (field unused so far, but serde default handles it)

**Step 4: Commit**

```bash
git add src/config.rs
git commit -m "feat: add instructions_file field to ClaudeConfig"
```

---

### Task 2: Add env var override for `CLAUDE_INSTRUCTIONS_FILE`

**Files:**
- Modify: `src/config.rs:649-653` (apply_env_overrides, Claude CLI section)

**Step 1: Write the failing test**

Add after the existing `test_env_override_claude_instructions` test (~line 2360):

```rust
#[test]
fn test_env_override_claude_instructions_file() {
    let yaml = r#"
work_dir: /tmp/repos
"#;
    let file = create_temp_yaml(yaml);

    with_env(&[("CLAUDE_INSTRUCTIONS_FILE", "./my-instructions.md")], || {
        let config = Config::load(file.path()).unwrap();
        assert_eq!(
            config.claude.instructions_file,
            Some("./my-instructions.md".to_string())
        );
    });
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test test_env_override_claude_instructions_file -- --nocapture 2>&1`
Expected: FAIL — env var not applied, field is None

**Step 3: Add the env override**

In `apply_env_overrides()`, after the `CLAUDE_INSTRUCTIONS` block (~line 653), add:

```rust
if let Ok(v) = env::var("CLAUDE_INSTRUCTIONS_FILE") {
    if !v.is_empty() {
        self.claude.instructions_file = Some(v);
    }
}
```

**Step 4: Run test to verify it passes**

Run: `cargo test test_env_override_claude_instructions_file -- --nocapture 2>&1`
Expected: PASS

**Step 5: Commit**

```bash
git add src/config.rs
git commit -m "feat: add CLAUDE_INSTRUCTIONS_FILE env var override"
```

---

### Task 3: Add `resolve_instructions_file` method to `Config`

**Files:**
- Modify: `src/config.rs` (add method to `impl Config` block, ~after line 548)

**Step 1: Write the failing tests**

Add these tests to the `#[cfg(test)]` module:

```rust
#[test]
fn test_resolve_instructions_file_reads_file() {
    let dir = tempfile::tempdir().unwrap();
    let instructions_path = dir.path().join("instructions.md");
    fs::write(&instructions_path, "Be helpful and concise.").unwrap();

    let yaml = format!(
        "work_dir: /tmp/repos\nclaude:\n  instructions_file: \"{}\"",
        instructions_path.display()
    );
    let config = Config::from_yaml(&yaml).unwrap();
    let resolved = config.resolve_instructions_file(dir.path()).unwrap();
    assert_eq!(resolved, Some("Be helpful and concise.".to_string()));
}

#[test]
fn test_resolve_instructions_file_relative_path() {
    let dir = tempfile::tempdir().unwrap();
    let instructions_path = dir.path().join("my-instructions.md");
    fs::write(&instructions_path, "Write tests first.").unwrap();

    let yaml = "work_dir: /tmp/repos\nclaude:\n  instructions_file: \"my-instructions.md\"";
    let config = Config::from_yaml(yaml).unwrap();
    let resolved = config.resolve_instructions_file(dir.path()).unwrap();
    assert_eq!(resolved, Some("Write tests first.".to_string()));
}

#[test]
fn test_resolve_instructions_file_combines_with_inline() {
    let dir = tempfile::tempdir().unwrap();
    let instructions_path = dir.path().join("base.md");
    fs::write(&instructions_path, "Base instructions from file.").unwrap();

    let yaml = "work_dir: /tmp/repos\nclaude:\n  instructions_file: \"base.md\"\n  instructions: \"Plus inline.\"";
    let config = Config::from_yaml(yaml).unwrap();
    let resolved = config.resolve_instructions_file(dir.path()).unwrap();
    assert_eq!(
        resolved,
        Some("Base instructions from file.\nPlus inline.".to_string())
    );
}

#[test]
fn test_resolve_instructions_file_inline_only() {
    let dir = tempfile::tempdir().unwrap();

    let yaml = "work_dir: /tmp/repos\nclaude:\n  instructions: \"Just inline.\"";
    let config = Config::from_yaml(yaml).unwrap();
    let resolved = config.resolve_instructions_file(dir.path()).unwrap();
    assert_eq!(resolved, Some("Just inline.".to_string()));
}

#[test]
fn test_resolve_instructions_file_neither_set() {
    let dir = tempfile::tempdir().unwrap();

    let yaml = "work_dir: /tmp/repos";
    let config = Config::from_yaml(yaml).unwrap();
    let resolved = config.resolve_instructions_file(dir.path()).unwrap();
    assert_eq!(resolved, None);
}

#[test]
fn test_resolve_instructions_file_not_found() {
    let dir = tempfile::tempdir().unwrap();

    let yaml = "work_dir: /tmp/repos\nclaude:\n  instructions_file: \"nonexistent.md\"";
    let config = Config::from_yaml(yaml).unwrap();
    let result = config.resolve_instructions_file(dir.path());
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("nonexistent.md"));
}

#[test]
fn test_resolve_instructions_file_empty_file() {
    let dir = tempfile::tempdir().unwrap();
    let instructions_path = dir.path().join("empty.md");
    fs::write(&instructions_path, "").unwrap();

    let yaml = "work_dir: /tmp/repos\nclaude:\n  instructions_file: \"empty.md\"";
    let config = Config::from_yaml(yaml).unwrap();
    let resolved = config.resolve_instructions_file(dir.path()).unwrap();
    // Empty file = no file instructions, but inline still works if present
    assert_eq!(resolved, None);
}
```

**Step 2: Run tests to verify they fail**

Run: `cargo test test_resolve_instructions_file -- --nocapture 2>&1`
Expected: FAIL — method does not exist

**Step 3: Implement the method**

Add to `impl Config` block (after `load` method, ~line 548):

```rust
/// Resolve `claude.instructions_file` by reading the file and combining
/// with inline `claude.instructions`.
///
/// - `config_dir`: directory containing the config file (for relative path resolution)
/// - File content comes first, then inline instructions appended with a newline
/// - Returns `None` if neither field is set
/// - Returns error if the file path is set but the file cannot be read
pub fn resolve_instructions_file(&self, config_dir: &Path) -> Result<Option<String>> {
    let file_content = if let Some(ref file_path) = self.claude.instructions_file {
        let path = Path::new(file_path);
        let resolved = if path.is_absolute() {
            path.to_path_buf()
        } else {
            config_dir.join(path)
        };
        let content = fs::read_to_string(&resolved).map_err(|e| {
            Error::config(format!(
                "Failed to read instructions file '{}': {}",
                resolved.display(),
                e
            ))
        })?;
        let trimmed = content.trim().to_string();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed)
        }
    } else {
        None
    };

    match (file_content, &self.claude.instructions) {
        (Some(file), Some(inline)) => Ok(Some(format!("{}\n{}", file, inline))),
        (Some(file), None) => Ok(Some(file)),
        (None, Some(inline)) => Ok(Some(inline.clone())),
        (None, None) => Ok(None),
    }
}
```

**Step 4: Run tests to verify they pass**

Run: `cargo test test_resolve_instructions_file -- --nocapture 2>&1`
Expected: All 7 tests PASS

**Step 5: Commit**

```bash
git add src/config.rs
git commit -m "feat: add resolve_instructions_file method to Config"
```

---

### Task 4: Integrate resolution into `Config::load`

**Files:**
- Modify: `src/config.rs:516-548` (Config::load method)
- Modify: `src/watcher.rs:73` (instructions field passthrough)

**Step 1: Write the failing test**

```rust
#[test]
fn test_load_resolves_instructions_file() {
    let dir = tempfile::tempdir().unwrap();
    let instructions_path = dir.path().join("my-instructions.md");
    fs::write(&instructions_path, "Instructions from file.").unwrap();

    let yaml = format!(
        "work_dir: /tmp/repos\nclaude:\n  instructions_file: \"my-instructions.md\"\n  instructions: \"And inline.\"",
    );
    let config_path = dir.path().join("claudear.yaml");
    fs::write(&config_path, &yaml).unwrap();

    with_env(&[], || {
        let config = Config::load(&config_path).unwrap();
        // After load, instructions should be the merged result
        assert_eq!(
            config.claude.instructions,
            Some("Instructions from file.\nAnd inline.".to_string())
        );
    });
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test test_load_resolves_instructions_file -- --nocapture 2>&1`
Expected: FAIL — instructions is just "And inline." (file not resolved)

**Step 3: Add resolution to Config::load**

In `Config::load` (line 516-548), after `apply_env_overrides()` and before `validate_project_config()`, add:

```rust
// Resolve instructions_file if set
let config_dir = path.parent().unwrap_or(Path::new("."));
let resolved_instructions = config.resolve_instructions_file(config_dir)?;
config.claude.instructions = resolved_instructions;
```

**Step 4: Run test to verify it passes**

Run: `cargo test test_load_resolves_instructions_file -- --nocapture 2>&1`
Expected: PASS

**Step 5: Run all tests**

Run: `cargo test 2>&1`
Expected: All tests PASS (existing behavior unchanged since `instructions_file` defaults to `None`)

**Step 6: Commit**

```bash
git add src/config.rs
git commit -m "feat: resolve instructions_file during config load"
```

---

### Task 5: Update example config and docs

**Files:**
- Modify: `claudear.example.yaml:98-99`

**Step 1: Update the example config**

After the existing `instructions` comment (~line 99), add the `instructions_file` documentation:

```yaml
  # Custom instructions appended to Claude's system prompt
  # instructions: "Always write tests. Follow existing code style."

  # Path to a file containing custom instructions (relative to this config file)
  # If both instructions and instructions_file are set, file content comes first
  # instructions_file: "./claude-instructions.md"
```

**Step 2: Run `cargo test` to make sure nothing broke**

Run: `cargo test 2>&1`
Expected: All tests PASS

**Step 3: Commit**

```bash
git add claudear.example.yaml
git commit -m "docs: add instructions_file to example config"
```

---

### Task 6: Final verification

**Step 1: Run full test suite**

Run: `cargo test 2>&1`
Expected: All tests PASS

**Step 2: Run clippy**

Run: `cargo clippy 2>&1`
Expected: No warnings related to our changes

**Step 3: Verify build**

Run: `cargo build 2>&1`
Expected: Builds cleanly
