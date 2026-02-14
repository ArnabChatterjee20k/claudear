# Dashboard Auth Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Add session-cookie authentication to the dashboard with full CRUD user management and CLI seeding.

**Architecture:** Server-side sessions stored in SQLite, bcrypt password hashing, HttpOnly cookies via tower-cookies. Axum extractors (`AuthUser`, `AdminUser`) protect all existing routes. React AuthContext on the frontend gates the entire app behind a login page.

**Tech Stack:** Rust (Axum, rusqlite, bcrypt, tower-cookies, rand), React (SWR, Tailwind), Bun (tests)

---

### Task 1: Add Rust dependencies

**Files:**
- Modify: `Cargo.toml`

**Step 1: Add bcrypt and tower-cookies to Cargo.toml**

Add these to the `[dependencies]` section:

```toml
# Authentication
bcrypt = "0.17"
tower-cookies = "0.10"
```

Note: `rand` is already in Cargo.toml. We'll use it for session token generation.

**Step 2: Verify it compiles**

Run: `cargo check`
Expected: compiles without errors

**Step 3: Commit**

```bash
git add Cargo.toml Cargo.lock
git commit -m "feat(auth): add bcrypt and tower-cookies dependencies"
```

---

### Task 2: Add users and sessions tables to SQLite schema

**Files:**
- Modify: `src/storage/sqlite.rs`

**Step 1: Add users and sessions table creation to `init()`**

In the `init()` method of `SqliteTracker`, after the existing `CREATE TABLE IF NOT EXISTS` block (after the `regression_checks` table, around line 501), add:

```rust
            -- ============================================================
            -- Authentication Tables
            -- ============================================================

            CREATE TABLE IF NOT EXISTS users (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                email TEXT NOT NULL UNIQUE,
                password_hash TEXT NOT NULL,
                name TEXT NOT NULL,
                role TEXT NOT NULL DEFAULT 'viewer',
                created_at TEXT NOT NULL DEFAULT (datetime('now')),
                updated_at TEXT NOT NULL DEFAULT (datetime('now'))
            );
            CREATE INDEX IF NOT EXISTS idx_users_email ON users(email);

            CREATE TABLE IF NOT EXISTS sessions (
                id TEXT PRIMARY KEY,
                user_id INTEGER NOT NULL,
                created_at TEXT NOT NULL DEFAULT (datetime('now')),
                expires_at TEXT NOT NULL,
                FOREIGN KEY (user_id) REFERENCES users(id) ON DELETE CASCADE
            );
            CREATE INDEX IF NOT EXISTS idx_sessions_expires ON sessions(expires_at);
            CREATE INDEX IF NOT EXISTS idx_sessions_user ON sessions(user_id);
```

**Step 2: Add user CRUD methods to SqliteTracker**

Add these methods to the `impl SqliteTracker` block:

```rust
    // ── User Management ─────────────────────────────────

    /// Create a new user. Returns the new user's ID.
    pub fn create_user(&self, email: &str, password_hash: &str, name: &str, role: &str) -> Result<i64> {
        let conn = self.acquire_lock()?;
        conn.execute(
            "INSERT INTO users (email, password_hash, name, role) VALUES (?1, ?2, ?3, ?4)",
            params![email, password_hash, name, role],
        )?;
        Ok(conn.last_insert_rowid())
    }

    /// Get a user by ID.
    pub fn get_user_by_id(&self, id: i64) -> Result<Option<UserRow>> {
        let conn = self.acquire_lock()?;
        let mut stmt = conn.prepare(
            "SELECT id, email, password_hash, name, role, created_at, updated_at FROM users WHERE id = ?1"
        )?;
        let user = stmt.query_row(params![id], UserRow::from_row).optional()?;
        Ok(user)
    }

    /// Get a user by email.
    pub fn get_user_by_email(&self, email: &str) -> Result<Option<UserRow>> {
        let conn = self.acquire_lock()?;
        let mut stmt = conn.prepare(
            "SELECT id, email, password_hash, name, role, created_at, updated_at FROM users WHERE email = ?1"
        )?;
        let user = stmt.query_row(params![email], UserRow::from_row).optional()?;
        Ok(user)
    }

    /// List all users (without password hashes).
    pub fn list_users(&self) -> Result<Vec<UserRow>> {
        let conn = self.acquire_lock()?;
        let mut stmt = conn.prepare(
            "SELECT id, email, password_hash, name, role, created_at, updated_at FROM users ORDER BY id"
        )?;
        let users = stmt.query_map([], UserRow::from_row)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(users)
    }

    /// Update a user. Only updates non-None fields.
    pub fn update_user(&self, id: i64, email: Option<&str>, password_hash: Option<&str>, name: Option<&str>, role: Option<&str>) -> Result<bool> {
        let conn = self.acquire_lock()?;
        let mut sets = Vec::new();
        let mut values: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();

        if let Some(e) = email {
            sets.push("email = ?");
            values.push(Box::new(e.to_string()));
        }
        if let Some(p) = password_hash {
            sets.push("password_hash = ?");
            values.push(Box::new(p.to_string()));
        }
        if let Some(n) = name {
            sets.push("name = ?");
            values.push(Box::new(n.to_string()));
        }
        if let Some(r) = role {
            sets.push("role = ?");
            values.push(Box::new(r.to_string()));
        }

        if sets.is_empty() {
            return Ok(false);
        }

        sets.push("updated_at = datetime('now')");
        values.push(Box::new(id));

        let sql = format!("UPDATE users SET {} WHERE id = ?", sets.join(", "));
        let params: Vec<&dyn rusqlite::types::ToSql> = values.iter().map(|v| v.as_ref()).collect();
        let rows = conn.execute(&sql, params.as_slice())?;
        Ok(rows > 0)
    }

    /// Delete a user by ID. Returns true if deleted.
    pub fn delete_user(&self, id: i64) -> Result<bool> {
        let conn = self.acquire_lock()?;
        let rows = conn.execute("DELETE FROM users WHERE id = ?1", params![id])?;
        Ok(rows > 0)
    }

    /// Count total users.
    pub fn count_users(&self) -> Result<i64> {
        let conn = self.acquire_lock()?;
        let count: i64 = conn.query_row("SELECT COUNT(*) FROM users", [], |row| row.get(0))?;
        Ok(count)
    }

    // ── Session Management ──────────────────────────────

    /// Create a new session. Returns the session token.
    pub fn create_session(&self, user_id: i64, expires_at: &str) -> Result<String> {
        let token = generate_session_token();
        let conn = self.acquire_lock()?;
        conn.execute(
            "INSERT INTO sessions (id, user_id, expires_at) VALUES (?1, ?2, ?3)",
            params![token, user_id, expires_at],
        )?;
        Ok(token)
    }

    /// Validate a session token. Returns the user if the session is valid and not expired.
    pub fn get_session_user(&self, token: &str) -> Result<Option<UserRow>> {
        let conn = self.acquire_lock()?;
        let mut stmt = conn.prepare(
            "SELECT u.id, u.email, u.password_hash, u.name, u.role, u.created_at, u.updated_at
             FROM sessions s
             JOIN users u ON s.user_id = u.id
             WHERE s.id = ?1 AND s.expires_at > datetime('now')"
        )?;
        let user = stmt.query_row(params![token], UserRow::from_row).optional()?;
        Ok(user)
    }

    /// Delete a session.
    pub fn delete_session(&self, token: &str) -> Result<()> {
        let conn = self.acquire_lock()?;
        conn.execute("DELETE FROM sessions WHERE id = ?1", params![token])?;
        Ok(())
    }

    /// Delete all expired sessions.
    pub fn cleanup_expired_sessions(&self) -> Result<usize> {
        let conn = self.acquire_lock()?;
        let deleted = conn.execute("DELETE FROM sessions WHERE expires_at <= datetime('now')", [])?;
        Ok(deleted)
    }

    /// Delete all sessions for a user.
    pub fn delete_user_sessions(&self, user_id: i64) -> Result<()> {
        let conn = self.acquire_lock()?;
        conn.execute("DELETE FROM sessions WHERE user_id = ?1", params![user_id])?;
        Ok(())
    }
```

**Step 3: Add the `UserRow` struct and `generate_session_token` function**

Add at the top of `sqlite.rs` (near other structs):

```rust
use rand::Rng;

/// A user row from the database.
#[derive(Debug, Clone, Serialize)]
pub struct UserRow {
    pub id: i64,
    pub email: String,
    #[serde(skip_serializing)]
    pub password_hash: String,
    pub name: String,
    pub role: String,
    pub created_at: String,
    pub updated_at: String,
}

impl UserRow {
    fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            id: row.get(0)?,
            email: row.get(1)?,
            password_hash: row.get(2)?,
            name: row.get(3)?,
            role: row.get(4)?,
            created_at: row.get(5)?,
            updated_at: row.get(6)?,
        })
    }
}

/// Generate a cryptographically random session token (64 hex chars = 32 bytes).
fn generate_session_token() -> String {
    let mut bytes = [0u8; 32];
    rand::rng().fill(&mut bytes);
    hex::encode(bytes)
}
```

Also add `UserRow` to the pub exports in `src/storage/mod.rs`:

```rust
pub use sqlite::{
    ConfidenceBreakdown, DiagnosticCounts, IndexStats, InferenceHistoryEntry, InferenceStats,
    SqliteTracker, StoredDependency, StoredIndexedRepo, StoredRepository, UserRow,
};
```

**Step 4: Add `optional()` import if not already present**

Ensure `use rusqlite::OptionalExtension;` is imported at the top of `sqlite.rs` (needed for `.optional()` on `query_row`). Check if it's already imported; if not, add it.

**Step 5: Write tests for user and session CRUD**

Add to the `#[cfg(test)] mod tests` block in `sqlite.rs`:

```rust
    #[test]
    fn test_create_and_get_user() {
        let tracker = SqliteTracker::in_memory().unwrap();
        let id = tracker.create_user("test@example.com", "$2b$12$hash", "Test User", "admin").unwrap();
        assert!(id > 0);

        let user = tracker.get_user_by_id(id).unwrap().unwrap();
        assert_eq!(user.email, "test@example.com");
        assert_eq!(user.name, "Test User");
        assert_eq!(user.role, "admin");
    }

    #[test]
    fn test_get_user_by_email() {
        let tracker = SqliteTracker::in_memory().unwrap();
        tracker.create_user("alice@example.com", "$2b$12$hash", "Alice", "viewer").unwrap();

        let user = tracker.get_user_by_email("alice@example.com").unwrap().unwrap();
        assert_eq!(user.name, "Alice");

        let missing = tracker.get_user_by_email("nobody@example.com").unwrap();
        assert!(missing.is_none());
    }

    #[test]
    fn test_list_users() {
        let tracker = SqliteTracker::in_memory().unwrap();
        tracker.create_user("a@test.com", "hash", "A", "admin").unwrap();
        tracker.create_user("b@test.com", "hash", "B", "viewer").unwrap();

        let users = tracker.list_users().unwrap();
        assert_eq!(users.len(), 2);
    }

    #[test]
    fn test_update_user() {
        let tracker = SqliteTracker::in_memory().unwrap();
        let id = tracker.create_user("old@test.com", "hash", "Old Name", "viewer").unwrap();

        tracker.update_user(id, Some("new@test.com"), None, Some("New Name"), Some("admin")).unwrap();

        let user = tracker.get_user_by_id(id).unwrap().unwrap();
        assert_eq!(user.email, "new@test.com");
        assert_eq!(user.name, "New Name");
        assert_eq!(user.role, "admin");
    }

    #[test]
    fn test_delete_user() {
        let tracker = SqliteTracker::in_memory().unwrap();
        let id = tracker.create_user("del@test.com", "hash", "Delete Me", "viewer").unwrap();

        assert!(tracker.delete_user(id).unwrap());
        assert!(tracker.get_user_by_id(id).unwrap().is_none());
        assert!(!tracker.delete_user(id).unwrap()); // already deleted
    }

    #[test]
    fn test_duplicate_email_fails() {
        let tracker = SqliteTracker::in_memory().unwrap();
        tracker.create_user("dup@test.com", "hash", "First", "admin").unwrap();

        let result = tracker.create_user("dup@test.com", "hash", "Second", "viewer");
        assert!(result.is_err());
    }

    #[test]
    fn test_create_and_validate_session() {
        let tracker = SqliteTracker::in_memory().unwrap();
        let user_id = tracker.create_user("sess@test.com", "hash", "Session User", "admin").unwrap();

        let token = tracker.create_session(user_id, "2099-12-31T23:59:59").unwrap();
        assert_eq!(token.len(), 64); // 32 bytes hex encoded

        let user = tracker.get_session_user(&token).unwrap().unwrap();
        assert_eq!(user.id, user_id);
        assert_eq!(user.email, "sess@test.com");
    }

    #[test]
    fn test_expired_session_returns_none() {
        let tracker = SqliteTracker::in_memory().unwrap();
        let user_id = tracker.create_user("exp@test.com", "hash", "Expired", "viewer").unwrap();

        let token = tracker.create_session(user_id, "2000-01-01T00:00:00").unwrap();

        let user = tracker.get_session_user(&token).unwrap();
        assert!(user.is_none());
    }

    #[test]
    fn test_delete_session() {
        let tracker = SqliteTracker::in_memory().unwrap();
        let user_id = tracker.create_user("delsess@test.com", "hash", "Del Sess", "admin").unwrap();

        let token = tracker.create_session(user_id, "2099-12-31T23:59:59").unwrap();
        assert!(tracker.get_session_user(&token).unwrap().is_some());

        tracker.delete_session(&token).unwrap();
        assert!(tracker.get_session_user(&token).unwrap().is_none());
    }

    #[test]
    fn test_cleanup_expired_sessions() {
        let tracker = SqliteTracker::in_memory().unwrap();
        let user_id = tracker.create_user("clean@test.com", "hash", "Clean", "admin").unwrap();

        tracker.create_session(user_id, "2000-01-01T00:00:00").unwrap();
        tracker.create_session(user_id, "2000-01-02T00:00:00").unwrap();
        tracker.create_session(user_id, "2099-12-31T23:59:59").unwrap();

        let deleted = tracker.cleanup_expired_sessions().unwrap();
        assert_eq!(deleted, 2);
    }

    #[test]
    fn test_count_users() {
        let tracker = SqliteTracker::in_memory().unwrap();
        assert_eq!(tracker.count_users().unwrap(), 0);

        tracker.create_user("c1@test.com", "hash", "C1", "admin").unwrap();
        tracker.create_user("c2@test.com", "hash", "C2", "viewer").unwrap();
        assert_eq!(tracker.count_users().unwrap(), 2);
    }
```

**Step 6: Run the tests**

Run: `cargo test --lib storage::sqlite::tests`
Expected: all new tests pass

**Step 7: Commit**

```bash
git add src/storage/sqlite.rs src/storage/mod.rs
git commit -m "feat(auth): add users and sessions tables with CRUD operations"
```

---

### Task 3: Add auth routes (login, logout, me)

**Files:**
- Create: `src/api/auth.rs`
- Modify: `src/api/mod.rs`
- Modify: `src/api/routes.rs`

**Step 1: Create `src/api/auth.rs` with auth extractors and handlers**

```rust
//! Authentication middleware and handlers.

use crate::storage::SqliteTracker;
use axum::{
    async_trait,
    extract::{FromRequestParts, State},
    http::{request::Parts, StatusCode},
    Json,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tower_cookies::{Cookie, Cookies};

use super::routes::ApiState;

const SESSION_COOKIE: &str = "claudear_session";
const SESSION_DURATION_DAYS: i64 = 7;

/// Authenticated user extracted from the session cookie.
/// Use this as an extractor in route handlers to require authentication.
#[derive(Debug, Clone, Serialize)]
pub struct AuthUser {
    pub id: i64,
    pub email: String,
    pub name: String,
    pub role: String,
}

/// Admin user extractor. Requires the user to have the "admin" role.
#[derive(Debug, Clone)]
pub struct AdminUser(pub AuthUser);

#[async_trait]
impl FromRequestParts<ApiState> for AuthUser {
    type Rejection = StatusCode;

    async fn from_request_parts(parts: &mut Parts, state: &ApiState) -> Result<Self, Self::Rejection> {
        let cookies = Cookies::from_request_parts(parts, state)
            .await
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

        let token = cookies
            .get(SESSION_COOKIE)
            .map(|c| c.value().to_string())
            .ok_or(StatusCode::UNAUTHORIZED)?;

        let user = state
            .tracker
            .as_any()
            .downcast_ref::<SqliteTracker>()
            .ok_or(StatusCode::INTERNAL_SERVER_ERROR)?
            .get_session_user(&token)
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
            .ok_or(StatusCode::UNAUTHORIZED)?;

        Ok(AuthUser {
            id: user.id,
            email: user.email,
            name: user.name,
            role: user.role,
        })
    }
}

#[async_trait]
impl FromRequestParts<ApiState> for AdminUser {
    type Rejection = StatusCode;

    async fn from_request_parts(parts: &mut Parts, state: &ApiState) -> Result<Self, Self::Rejection> {
        let user = AuthUser::from_request_parts(parts, state).await?;
        if user.role != "admin" {
            return Err(StatusCode::FORBIDDEN);
        }
        Ok(AdminUser(user))
    }
}

// ── Request/Response types ──────────────────────────

#[derive(Deserialize)]
pub struct LoginRequest {
    pub email: String,
    pub password: String,
}

#[derive(Serialize)]
pub struct LoginResponse {
    pub user: AuthUser,
}

#[derive(Serialize)]
pub struct UserResponse {
    pub id: i64,
    pub email: String,
    pub name: String,
    pub role: String,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Deserialize)]
pub struct CreateUserRequest {
    pub email: String,
    pub password: String,
    pub name: String,
    #[serde(default = "default_role")]
    pub role: String,
}

fn default_role() -> String {
    "viewer".to_string()
}

#[derive(Deserialize)]
pub struct UpdateUserRequest {
    pub email: Option<String>,
    pub password: Option<String>,
    pub name: Option<String>,
    pub role: Option<String>,
}

// ── Handlers ────────────────────────────────────────

pub async fn login_handler(
    State(state): State<ApiState>,
    cookies: Cookies,
    Json(body): Json<LoginRequest>,
) -> Result<Json<LoginResponse>, StatusCode> {
    let tracker = state
        .tracker
        .as_any()
        .downcast_ref::<SqliteTracker>()
        .ok_or(StatusCode::INTERNAL_SERVER_ERROR)?;

    // Clean up expired sessions opportunistically
    let _ = tracker.cleanup_expired_sessions();

    let user = tracker
        .get_user_by_email(&body.email)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::UNAUTHORIZED)?;

    let password_valid = bcrypt::verify(&body.password, &user.password_hash)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    if !password_valid {
        return Err(StatusCode::UNAUTHORIZED);
    }

    let expires_at = chrono::Utc::now()
        + chrono::Duration::days(SESSION_DURATION_DAYS);
    let expires_str = expires_at.format("%Y-%m-%dT%H:%M:%S").to_string();

    let token = tracker
        .create_session(user.id, &expires_str)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let mut cookie = Cookie::new(SESSION_COOKIE, token);
    cookie.set_path("/");
    cookie.set_http_only(true);
    cookie.set_same_site(tower_cookies::cookie::SameSite::Lax);
    cookie.set_max_age(tower_cookies::cookie::time::Duration::days(SESSION_DURATION_DAYS));
    cookies.add(cookie);

    Ok(Json(LoginResponse {
        user: AuthUser {
            id: user.id,
            email: user.email,
            name: user.name,
            role: user.role,
        },
    }))
}

pub async fn logout_handler(
    State(state): State<ApiState>,
    cookies: Cookies,
) -> StatusCode {
    if let Some(token) = cookies.get(SESSION_COOKIE) {
        if let Some(tracker) = state.tracker.as_any().downcast_ref::<SqliteTracker>() {
            let _ = tracker.delete_session(token.value());
        }
    }

    let mut cookie = Cookie::new(SESSION_COOKIE, "");
    cookie.set_path("/");
    cookie.set_max_age(tower_cookies::cookie::time::Duration::ZERO);
    cookies.add(cookie);

    StatusCode::OK
}

pub async fn me_handler(user: AuthUser) -> Json<AuthUser> {
    Json(user)
}

// ── User CRUD handlers (admin only) ────────────────

pub async fn list_users_handler(
    State(state): State<ApiState>,
    _admin: AdminUser,
) -> Result<Json<Vec<UserResponse>>, StatusCode> {
    let tracker = state.tracker.as_any().downcast_ref::<SqliteTracker>()
        .ok_or(StatusCode::INTERNAL_SERVER_ERROR)?;

    let users = tracker.list_users().map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    Ok(Json(users.into_iter().map(|u| UserResponse {
        id: u.id, email: u.email, name: u.name, role: u.role,
        created_at: u.created_at, updated_at: u.updated_at,
    }).collect()))
}

pub async fn get_user_handler(
    State(state): State<ApiState>,
    _admin: AdminUser,
    axum::extract::Path(id): axum::extract::Path<i64>,
) -> Result<Json<UserResponse>, StatusCode> {
    let tracker = state.tracker.as_any().downcast_ref::<SqliteTracker>()
        .ok_or(StatusCode::INTERNAL_SERVER_ERROR)?;

    let user = tracker.get_user_by_id(id)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::NOT_FOUND)?;

    Ok(Json(UserResponse {
        id: user.id, email: user.email, name: user.name, role: user.role,
        created_at: user.created_at, updated_at: user.updated_at,
    }))
}

pub async fn create_user_handler(
    State(state): State<ApiState>,
    _admin: AdminUser,
    Json(body): Json<CreateUserRequest>,
) -> Result<(StatusCode, Json<UserResponse>), StatusCode> {
    let tracker = state.tracker.as_any().downcast_ref::<SqliteTracker>()
        .ok_or(StatusCode::INTERNAL_SERVER_ERROR)?;

    if body.role != "admin" && body.role != "viewer" {
        return Err(StatusCode::BAD_REQUEST);
    }

    let hash = bcrypt::hash(&body.password, bcrypt::DEFAULT_COST)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let id = tracker.create_user(&body.email, &hash, &body.name, &body.role)
        .map_err(|_| StatusCode::CONFLICT)?;

    let user = tracker.get_user_by_id(id)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::INTERNAL_SERVER_ERROR)?;

    Ok((StatusCode::CREATED, Json(UserResponse {
        id: user.id, email: user.email, name: user.name, role: user.role,
        created_at: user.created_at, updated_at: user.updated_at,
    })))
}

pub async fn update_user_handler(
    State(state): State<ApiState>,
    _admin: AdminUser,
    axum::extract::Path(id): axum::extract::Path<i64>,
    Json(body): Json<UpdateUserRequest>,
) -> Result<Json<UserResponse>, StatusCode> {
    let tracker = state.tracker.as_any().downcast_ref::<SqliteTracker>()
        .ok_or(StatusCode::INTERNAL_SERVER_ERROR)?;

    if let Some(ref role) = body.role {
        if role != "admin" && role != "viewer" {
            return Err(StatusCode::BAD_REQUEST);
        }
    }

    let password_hash = match &body.password {
        Some(pw) => Some(
            bcrypt::hash(pw, bcrypt::DEFAULT_COST)
                .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        ),
        None => None,
    };

    tracker.update_user(
        id,
        body.email.as_deref(),
        password_hash.as_deref(),
        body.name.as_deref(),
        body.role.as_deref(),
    ).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let user = tracker.get_user_by_id(id)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::NOT_FOUND)?;

    Ok(Json(UserResponse {
        id: user.id, email: user.email, name: user.name, role: user.role,
        created_at: user.created_at, updated_at: user.updated_at,
    }))
}

pub async fn delete_user_handler(
    State(state): State<ApiState>,
    admin: AdminUser,
    axum::extract::Path(id): axum::extract::Path<i64>,
) -> Result<StatusCode, StatusCode> {
    if admin.0.id == id {
        return Err(StatusCode::BAD_REQUEST); // Can't delete yourself
    }

    let tracker = state.tracker.as_any().downcast_ref::<SqliteTracker>()
        .ok_or(StatusCode::INTERNAL_SERVER_ERROR)?;

    let deleted = tracker.delete_user(id)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    if deleted { Ok(StatusCode::NO_CONTENT) } else { Err(StatusCode::NOT_FOUND) }
}
```

**Step 2: Add `as_any()` to the `FixAttemptTracker` trait**

In `src/storage/mod.rs`, add to the trait:

```rust
pub trait FixAttemptTracker: Send + Sync {
    /// Downcast to concrete type for auth operations.
    fn as_any(&self) -> &dyn std::any::Any;
    // ... rest of existing methods
}
```

In `src/storage/sqlite.rs`, add to the `impl FixAttemptTracker for SqliteTracker` block:

```rust
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
```

**Step 3: Wire auth routes into the router**

In `src/api/routes.rs`, update the `create_api_router_with_dashboard` function:

Add auth routes that don't require authentication:
```rust
use super::auth::*;

// Add to the router builder, BEFORE .with_state(state):
.route("/api/auth/login", axum::routing::post(login_handler))
.route("/api/auth/logout", axum::routing::post(logout_handler))
.route("/api/auth/me", axum::routing::get(me_handler))

// User CRUD routes (admin only)
.route("/api/users", axum::routing::get(list_users_handler).post(create_user_handler))
.route("/api/users/{id}", axum::routing::get(get_user_handler).put(update_user_handler).delete(delete_user_handler))
```

**Step 4: Add tower-cookies layer to the API server**

In `src/api/mod.rs`, update the `start()` method to add the cookies layer:

```rust
use tower_cookies::CookieManagerLayer;

// In start(), add the cookie layer:
let app = create_api_router_with_dashboard(
    self.config.clone(),
    self.tracker.clone(),
    self.dashboard_dir.clone(),
)
.layer(cors)
.layer(CookieManagerLayer::new());
```

**Step 5: Add AuthUser extractor to all existing route handlers**

In `src/api/routes.rs`, add `_user: AuthUser` as the first parameter to every existing handler:

```rust
async fn health_handler(
    _user: AuthUser,
    State(state): State<ApiState>,
) -> impl IntoResponse { ... }

async fn stats_handler(
    _user: AuthUser,
    State(state): State<ApiState>,
) -> ... { ... }

// ... repeat for ALL existing handlers
```

Note: The `AuthUser` extractor parameter must come BEFORE `State<ApiState>` in the handler signature because Axum processes extractors left-to-right.

**Step 6: Update `src/api/mod.rs` to declare the auth module**

```rust
pub mod auth;
mod routes;
```

**Step 7: Run cargo check**

Run: `cargo check`
Expected: compiles

**Step 8: Commit**

```bash
git add src/api/auth.rs src/api/mod.rs src/api/routes.rs src/storage/mod.rs src/storage/sqlite.rs
git commit -m "feat(auth): add auth middleware, login/logout/me endpoints, and user CRUD routes"
```

---

### Task 4: Add `claudear users seed` CLI command

**Files:**
- Modify: `src/main.rs`

**Step 1: Add `Users` subcommand to the CLI**

Add to the `Commands` enum:

```rust
    /// User management commands
    #[command(subcommand)]
    Users(UsersCommands),
```

Add the subcommand enum:

```rust
/// User management subcommands
#[derive(Subcommand)]
enum UsersCommands {
    /// Seed an admin user (creates or updates password if email exists)
    Seed {
        /// User email
        #[arg(long)]
        email: String,

        /// User password
        #[arg(long)]
        password: String,

        /// User display name
        #[arg(long, default_value = "Admin")]
        name: String,
    },
}
```

**Step 2: Handle the command in the match block**

Add to the `match cli.command` block:

```rust
        Commands::Users(cmd) => match cmd {
            UsersCommands::Seed { email, password, name } => {
                let tracker = SqliteTracker::new(&config.db_path)?;

                let hash = bcrypt::hash(&password, bcrypt::DEFAULT_COST)
                    .map_err(|e| claudear::Error::Other(format!("Failed to hash password: {}", e)))?;

                // Check if user already exists
                match tracker.get_user_by_email(&email)? {
                    Some(existing) => {
                        tracker.update_user(existing.id, None, Some(&hash), Some(&name), Some("admin"))?;
                        println!("Updated existing user '{}' (id={}) with new password and admin role", email, existing.id);
                    }
                    None => {
                        let id = tracker.create_user(&email, &hash, &name, "admin")?;
                        println!("Created admin user '{}' (id={})", email, id);
                    }
                }
            }
        },
```

**Step 3: Add bcrypt import to main.rs**

At the top of `main.rs`, no new import needed — bcrypt is used directly as `bcrypt::hash`.

**Step 4: Run cargo check**

Run: `cargo check`
Expected: compiles

**Step 5: Commit**

```bash
git add src/main.rs
git commit -m "feat(auth): add 'claudear users seed' CLI command for initial admin setup"
```

---

### Task 5: Update frontend API client for auth

**Files:**
- Modify: `dashboard/src/lib/api.ts`

**Step 1: Update `fetchJson` to handle 401 responses**

Replace the existing `fetchJson` function and add auth API functions:

```typescript
// ─── Auth types ──────────────────────────────────

export interface AuthUser {
  id: number;
  email: string;
  name: string;
  role: string;
}

export interface LoginResponse {
  user: AuthUser;
}

export interface UserRecord {
  id: number;
  email: string;
  name: string;
  role: string;
  created_at: string;
  updated_at: string;
}

// ─── Fetchers ──────────────────────────────────

let onUnauthorized: (() => void) | null = null;

export function setOnUnauthorized(cb: () => void) {
  onUnauthorized = cb;
}

async function fetchJson<T>(url: string): Promise<T> {
  const res = await fetch(url);
  if (res.status === 401) {
    onUnauthorized?.();
    throw new Error('Unauthorized');
  }
  if (!res.ok) throw new Error(`Failed to fetch ${url}: ${res.status}`);
  return res.json();
}

async function postJson<T>(url: string, body?: unknown): Promise<T> {
  const res = await fetch(url, {
    method: 'POST',
    headers: body ? { 'Content-Type': 'application/json' } : {},
    body: body ? JSON.stringify(body) : undefined,
  });
  if (res.status === 401) {
    onUnauthorized?.();
    throw new Error('Unauthorized');
  }
  if (!res.ok) throw new Error(`Failed to post ${url}: ${res.status}`);
  if (res.status === 204) return undefined as T;
  return res.json();
}

async function putJson<T>(url: string, body: unknown): Promise<T> {
  const res = await fetch(url, {
    method: 'PUT',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify(body),
  });
  if (res.status === 401) {
    onUnauthorized?.();
    throw new Error('Unauthorized');
  }
  if (!res.ok) throw new Error(`Failed to put ${url}: ${res.status}`);
  return res.json();
}

async function deleteRequest(url: string): Promise<void> {
  const res = await fetch(url, { method: 'DELETE' });
  if (res.status === 401) {
    onUnauthorized?.();
    throw new Error('Unauthorized');
  }
  if (!res.ok) throw new Error(`Failed to delete ${url}: ${res.status}`);
}

// ─── Auth API ────────────────────────────────────

export async function login(email: string, password: string): Promise<LoginResponse> {
  return postJson(`${API_BASE}/auth/login`, { email, password });
}

export async function logout(): Promise<void> {
  await postJson(`${API_BASE}/auth/logout`);
}

export async function getMe(): Promise<AuthUser> {
  return fetchJson(`${API_BASE}/auth/me`);
}

// ─── User Management API ─────────────────────────

export async function fetchUsers(): Promise<UserRecord[]> {
  return fetchJson(`${API_BASE}/users`);
}

export async function getUser(id: number): Promise<UserRecord> {
  return fetchJson(`${API_BASE}/users/${id}`);
}

export async function createUser(data: {
  email: string; password: string; name: string; role: string;
}): Promise<UserRecord> {
  return postJson(`${API_BASE}/users`, data);
}

export async function updateUser(id: number, data: {
  email?: string; password?: string; name?: string; role?: string;
}): Promise<UserRecord> {
  return putJson(`${API_BASE}/users/${id}`, data);
}

export async function deleteUser(id: number): Promise<void> {
  return deleteRequest(`${API_BASE}/users/${id}`);
}
```

**Step 2: Commit**

```bash
git add dashboard/src/lib/api.ts
git commit -m "feat(auth): add auth and user management API functions to frontend client"
```

---

### Task 6: Add AuthProvider and login page to frontend

**Files:**
- Create: `dashboard/src/lib/auth.tsx`
- Create: `dashboard/src/pages/login.tsx`
- Modify: `dashboard/src/App.tsx`

**Step 1: Create `dashboard/src/lib/auth.tsx`**

```tsx
import { createContext, useContext, useState, useEffect, useCallback } from 'react'
import { getMe, login as apiLogin, logout as apiLogout, setOnUnauthorized, type AuthUser } from './api'

interface AuthState {
  user: AuthUser | null
  loading: boolean
  login: (email: string, password: string) => Promise<void>
  logout: () => Promise<void>
}

const AuthContext = createContext<AuthState>({
  user: null,
  loading: true,
  login: async () => {},
  logout: async () => {},
})

export function useAuth() {
  return useContext(AuthContext)
}

export function AuthProvider({ children }: { children: React.ReactNode }) {
  const [user, setUser] = useState<AuthUser | null>(null)
  const [loading, setLoading] = useState(true)

  const handleUnauthorized = useCallback(() => {
    setUser(null)
  }, [])

  useEffect(() => {
    setOnUnauthorized(handleUnauthorized)
    getMe()
      .then(setUser)
      .catch(() => setUser(null))
      .finally(() => setLoading(false))
  }, [handleUnauthorized])

  const login = useCallback(async (email: string, password: string) => {
    const res = await apiLogin(email, password)
    setUser(res.user)
  }, [])

  const logout = useCallback(async () => {
    await apiLogout()
    setUser(null)
  }, [])

  return (
    <AuthContext.Provider value={{ user, loading, login, logout }}>
      {children}
    </AuthContext.Provider>
  )
}
```

**Step 2: Create `dashboard/src/pages/login.tsx`**

```tsx
import { useState } from 'react'
import { useAuth } from '../lib/auth'
import { Activity } from 'lucide-react'

export default function LoginPage() {
  const { login } = useAuth()
  const [email, setEmail] = useState('')
  const [password, setPassword] = useState('')
  const [error, setError] = useState('')
  const [loading, setLoading] = useState(false)

  const handleSubmit = async (e: React.FormEvent) => {
    e.preventDefault()
    setError('')
    setLoading(true)
    try {
      await login(email, password)
    } catch {
      setError('Invalid email or password')
    } finally {
      setLoading(false)
    }
  }

  return (
    <div className="min-h-screen bg-background flex items-center justify-center">
      <div className="w-full max-w-sm space-y-6">
        <div className="text-center space-y-2">
          <Activity className="h-10 w-10 text-primary mx-auto" />
          <h1 className="text-2xl font-bold">Claudear</h1>
          <p className="text-sm text-muted-foreground">Sign in to your dashboard</p>
        </div>
        <form onSubmit={handleSubmit} className="space-y-4">
          {error && (
            <div className="bg-destructive/10 text-destructive text-sm p-3 rounded-md">
              {error}
            </div>
          )}
          <div className="space-y-2">
            <label htmlFor="email" className="text-sm font-medium">Email</label>
            <input
              id="email"
              type="email"
              value={email}
              onChange={(e) => setEmail(e.target.value)}
              required
              autoFocus
              className="w-full px-3 py-2 border rounded-md bg-background text-sm focus:outline-none focus:ring-2 focus:ring-primary"
              placeholder="admin@example.com"
            />
          </div>
          <div className="space-y-2">
            <label htmlFor="password" className="text-sm font-medium">Password</label>
            <input
              id="password"
              type="password"
              value={password}
              onChange={(e) => setPassword(e.target.value)}
              required
              className="w-full px-3 py-2 border rounded-md bg-background text-sm focus:outline-none focus:ring-2 focus:ring-primary"
            />
          </div>
          <button
            type="submit"
            disabled={loading}
            className="w-full py-2 px-4 bg-primary text-primary-foreground rounded-md text-sm font-medium hover:bg-primary/90 disabled:opacity-50"
          >
            {loading ? 'Signing in...' : 'Sign in'}
          </button>
        </form>
      </div>
    </div>
  )
}
```

**Step 3: Update `dashboard/src/App.tsx` to wrap with AuthProvider and gate on auth**

```tsx
import { Router } from './router'
import { AppShell } from './components/layout/app-shell'
import { AuthProvider, useAuth } from './lib/auth'
import LoginPage from './pages/login'
import OverviewPage from './pages/overview'
import AttemptsPage from './pages/attempts'
import PrsPage from './pages/prs'
import AnalyticsPage from './pages/analytics'
import ErrorsPage from './pages/errors'
import FeedbackPage from './pages/feedback'
import RegressionsPage from './pages/regressions'
import ExperimentsPage from './pages/experiments'
import ReposPage from './pages/repos'
import InferencePage from './pages/inference'
import ActivityPage from './pages/activity'
import UsersPage from './pages/users'

const routes: Record<string, () => JSX.Element> = {
  '/': OverviewPage,
  '/attempts': AttemptsPage,
  '/prs': PrsPage,
  '/analytics': AnalyticsPage,
  '/errors': ErrorsPage,
  '/feedback': FeedbackPage,
  '/regressions': RegressionsPage,
  '/experiments': ExperimentsPage,
  '/repos': ReposPage,
  '/inference': InferencePage,
  '/activity': ActivityPage,
  '/users': UsersPage,
}

function AuthenticatedApp() {
  const { user, loading } = useAuth()

  if (loading) {
    return (
      <div className="min-h-screen bg-background flex items-center justify-center">
        <div className="text-muted-foreground text-sm">Loading...</div>
      </div>
    )
  }

  if (!user) {
    return <LoginPage />
  }

  return (
    <AppShell>
      <Router routes={routes} />
    </AppShell>
  )
}

function App() {
  return (
    <AuthProvider>
      <AuthenticatedApp />
    </AuthProvider>
  )
}

export default App
```

**Step 4: Commit**

```bash
git add dashboard/src/lib/auth.tsx dashboard/src/pages/login.tsx dashboard/src/App.tsx
git commit -m "feat(auth): add AuthProvider, login page, and auth gating to frontend"
```

---

### Task 7: Add user management page and update sidebar

**Files:**
- Create: `dashboard/src/pages/users.tsx`
- Modify: `dashboard/src/components/layout/sidebar.tsx`
- Modify: `dashboard/src/components/layout/app-shell.tsx`

**Step 1: Create `dashboard/src/pages/users.tsx`**

```tsx
import { useState, useEffect, useCallback } from 'react'
import { fetchUsers, createUser, updateUser, deleteUser, type UserRecord } from '../lib/api'
import { useAuth } from '../lib/auth'
import { Plus, Pencil, Trash2, X } from 'lucide-react'

export default function UsersPage() {
  const { user: currentUser } = useAuth()
  const [users, setUsers] = useState<UserRecord[]>([])
  const [loading, setLoading] = useState(true)
  const [showForm, setShowForm] = useState(false)
  const [editingUser, setEditingUser] = useState<UserRecord | null>(null)
  const [error, setError] = useState('')

  const loadUsers = useCallback(async () => {
    try {
      const data = await fetchUsers()
      setUsers(data)
    } catch {
      setError('Failed to load users')
    } finally {
      setLoading(false)
    }
  }, [])

  useEffect(() => { loadUsers() }, [loadUsers])

  if (currentUser?.role !== 'admin') {
    return <div className="text-muted-foreground text-sm">You don't have permission to manage users.</div>
  }

  const handleDelete = async (id: number) => {
    if (!confirm('Delete this user?')) return
    try {
      await deleteUser(id)
      await loadUsers()
    } catch {
      setError('Failed to delete user')
    }
  }

  return (
    <div className="space-y-6">
      <div className="flex items-center justify-between">
        <h2 className="text-2xl font-bold">Users</h2>
        <button
          onClick={() => { setEditingUser(null); setShowForm(true) }}
          className="flex items-center gap-2 px-3 py-2 bg-primary text-primary-foreground rounded-md text-sm font-medium hover:bg-primary/90"
        >
          <Plus className="h-4 w-4" /> Add User
        </button>
      </div>

      {error && (
        <div className="bg-destructive/10 text-destructive text-sm p-3 rounded-md">{error}</div>
      )}

      {showForm && (
        <UserForm
          user={editingUser}
          onSave={async () => { setShowForm(false); await loadUsers() }}
          onCancel={() => setShowForm(false)}
        />
      )}

      {loading ? (
        <div className="text-muted-foreground text-sm">Loading...</div>
      ) : (
        <div className="border rounded-lg overflow-hidden">
          <table className="w-full text-sm">
            <thead className="bg-muted/50">
              <tr>
                <th className="text-left p-3 font-medium">Name</th>
                <th className="text-left p-3 font-medium">Email</th>
                <th className="text-left p-3 font-medium">Role</th>
                <th className="text-left p-3 font-medium">Created</th>
                <th className="text-right p-3 font-medium">Actions</th>
              </tr>
            </thead>
            <tbody>
              {users.map((u) => (
                <tr key={u.id} className="border-t">
                  <td className="p-3">{u.name}</td>
                  <td className="p-3 text-muted-foreground">{u.email}</td>
                  <td className="p-3">
                    <span className={`px-2 py-0.5 rounded-full text-xs font-medium ${
                      u.role === 'admin' ? 'bg-primary/10 text-primary' : 'bg-muted text-muted-foreground'
                    }`}>{u.role}</span>
                  </td>
                  <td className="p-3 text-muted-foreground">{new Date(u.created_at).toLocaleDateString()}</td>
                  <td className="p-3 text-right space-x-1">
                    <button
                      onClick={() => { setEditingUser(u); setShowForm(true) }}
                      className="p-1.5 rounded hover:bg-muted"
                      title="Edit"
                    >
                      <Pencil className="h-4 w-4" />
                    </button>
                    {u.id !== currentUser?.id && (
                      <button
                        onClick={() => handleDelete(u.id)}
                        className="p-1.5 rounded hover:bg-destructive/10 text-destructive"
                        title="Delete"
                      >
                        <Trash2 className="h-4 w-4" />
                      </button>
                    )}
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      )}
    </div>
  )
}

function UserForm({
  user,
  onSave,
  onCancel,
}: {
  user: UserRecord | null
  onSave: () => void
  onCancel: () => void
}) {
  const [email, setEmail] = useState(user?.email ?? '')
  const [name, setName] = useState(user?.name ?? '')
  const [password, setPassword] = useState('')
  const [role, setRole] = useState(user?.role ?? 'viewer')
  const [error, setError] = useState('')
  const [saving, setSaving] = useState(false)

  const handleSubmit = async (e: React.FormEvent) => {
    e.preventDefault()
    setError('')
    setSaving(true)
    try {
      if (user) {
        await updateUser(user.id, {
          email: email !== user.email ? email : undefined,
          name: name !== user.name ? name : undefined,
          role: role !== user.role ? role : undefined,
          password: password || undefined,
        })
      } else {
        if (!password) { setError('Password is required'); setSaving(false); return }
        await createUser({ email, password, name, role })
      }
      onSave()
    } catch {
      setError(user ? 'Failed to update user' : 'Failed to create user')
    } finally {
      setSaving(false)
    }
  }

  return (
    <div className="border rounded-lg p-4 bg-card">
      <div className="flex items-center justify-between mb-4">
        <h3 className="font-medium">{user ? 'Edit User' : 'New User'}</h3>
        <button onClick={onCancel} className="p-1 rounded hover:bg-muted">
          <X className="h-4 w-4" />
        </button>
      </div>
      {error && (
        <div className="bg-destructive/10 text-destructive text-sm p-3 rounded-md mb-4">{error}</div>
      )}
      <form onSubmit={handleSubmit} className="grid grid-cols-2 gap-4">
        <div className="space-y-1">
          <label className="text-sm font-medium">Name</label>
          <input
            value={name} onChange={(e) => setName(e.target.value)} required
            className="w-full px-3 py-2 border rounded-md bg-background text-sm focus:outline-none focus:ring-2 focus:ring-primary"
          />
        </div>
        <div className="space-y-1">
          <label className="text-sm font-medium">Email</label>
          <input
            type="email" value={email} onChange={(e) => setEmail(e.target.value)} required
            className="w-full px-3 py-2 border rounded-md bg-background text-sm focus:outline-none focus:ring-2 focus:ring-primary"
          />
        </div>
        <div className="space-y-1">
          <label className="text-sm font-medium">Password{user ? ' (leave blank to keep)' : ''}</label>
          <input
            type="password" value={password} onChange={(e) => setPassword(e.target.value)}
            required={!user}
            className="w-full px-3 py-2 border rounded-md bg-background text-sm focus:outline-none focus:ring-2 focus:ring-primary"
          />
        </div>
        <div className="space-y-1">
          <label className="text-sm font-medium">Role</label>
          <select
            value={role} onChange={(e) => setRole(e.target.value)}
            className="w-full px-3 py-2 border rounded-md bg-background text-sm focus:outline-none focus:ring-2 focus:ring-primary"
          >
            <option value="admin">Admin</option>
            <option value="viewer">Viewer</option>
          </select>
        </div>
        <div className="col-span-2 flex justify-end gap-2">
          <button type="button" onClick={onCancel} className="px-3 py-2 border rounded-md text-sm hover:bg-muted">
            Cancel
          </button>
          <button type="submit" disabled={saving} className="px-3 py-2 bg-primary text-primary-foreground rounded-md text-sm font-medium hover:bg-primary/90 disabled:opacity-50">
            {saving ? 'Saving...' : user ? 'Update' : 'Create'}
          </button>
        </div>
      </form>
    </div>
  )
}
```

**Step 2: Update sidebar to show Users link for admins and add logout**

Modify `dashboard/src/components/layout/sidebar.tsx`:

- Import `useAuth` from `../../lib/auth`
- Add `Users` icon import from lucide-react
- Add conditional Users nav item for admins
- Add user info and logout button at the bottom of the sidebar

```tsx
import { useRouter } from '../../router'
import { useAuth } from '../../lib/auth'
import {
  Activity, BarChart3, AlertTriangle, MessageSquare, Shield, FlaskConical,
  FolderGit2, Brain, ScrollText, LayoutDashboard, ListChecks, GitPullRequest,
  Users, LogOut,
} from 'lucide-react'

const navItems = [
  { path: '/', label: 'Overview', icon: LayoutDashboard },
  { path: '/attempts', label: 'Attempts', icon: ListChecks },
  { path: '/prs', label: 'PRs', icon: GitPullRequest },
  { path: '/analytics', label: 'Analytics', icon: BarChart3 },
  { path: '/errors', label: 'Errors', icon: AlertTriangle },
  { path: '/feedback', label: 'Feedback', icon: MessageSquare },
  { path: '/regressions', label: 'Regressions', icon: Shield },
  { path: '/experiments', label: 'Experiments', icon: FlaskConical },
  { path: '/repos', label: 'Repos', icon: FolderGit2 },
  { path: '/inference', label: 'Inference', icon: Brain },
  { path: '/activity', label: 'Activity', icon: ScrollText },
] as const

export function Sidebar() {
  const { path, navigate } = useRouter()
  const { user, logout } = useAuth()

  return (
    <aside className="w-56 border-r bg-card flex flex-col">
      <div className="p-4 border-b">
        <h1 className="text-lg font-bold flex items-center gap-2">
          <Activity className="h-5 w-5 text-primary" />
          Claudear
        </h1>
      </div>
      <nav className="flex-1 p-2 space-y-0.5 overflow-y-auto">
        {navItems.map(({ path: itemPath, label, icon: Icon }) => {
          const isActive = path === itemPath
          return (
            <button
              key={itemPath}
              onClick={() => navigate(itemPath)}
              className={`w-full flex items-center gap-2 px-3 py-2 rounded-md text-sm transition-colors ${
                isActive
                  ? 'bg-primary/10 text-primary font-medium'
                  : 'text-muted-foreground hover:bg-muted hover:text-foreground'
              }`}
            >
              <Icon className="h-4 w-4 shrink-0" />
              {label}
            </button>
          )
        })}
        {user?.role === 'admin' && (
          <button
            onClick={() => navigate('/users')}
            className={`w-full flex items-center gap-2 px-3 py-2 rounded-md text-sm transition-colors ${
              path === '/users'
                ? 'bg-primary/10 text-primary font-medium'
                : 'text-muted-foreground hover:bg-muted hover:text-foreground'
            }`}
          >
            <Users className="h-4 w-4 shrink-0" />
            Users
          </button>
        )}
      </nav>
      <div className="p-3 border-t">
        <div className="flex items-center justify-between">
          <div className="min-w-0">
            <div className="text-sm font-medium truncate">{user?.name}</div>
            <div className="text-xs text-muted-foreground truncate">{user?.email}</div>
          </div>
          <button
            onClick={logout}
            className="p-1.5 rounded hover:bg-muted text-muted-foreground"
            title="Sign out"
          >
            <LogOut className="h-4 w-4" />
          </button>
        </div>
      </div>
    </aside>
  )
}
```

**Step 3: Commit**

```bash
git add dashboard/src/pages/users.tsx dashboard/src/components/layout/sidebar.tsx
git commit -m "feat(auth): add user management page and auth UI to sidebar"
```

---

### Task 8: Update existing backend tests

**Files:**
- Modify: `src/api/routes.rs` (test module)

**Step 1: Update test helpers to include auth**

The existing tests create a router without authentication. Since all routes now require `AuthUser`, the tests need to either:
1. Create a test user + session and include the session cookie in requests
2. Or create a helper that sets up auth for tests

Update the test module in `routes.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{...}; // existing imports
    use crate::storage::SqliteTracker;
    use axum::body::Body;
    use axum::http::Request;
    use http_body_util::BodyExt;
    use tower::ServiceExt;
    use tower_cookies::CookieManagerLayer;

    // ... existing test_config() and create_test_tracker() ...

    /// Create a test router with the cookie layer.
    fn create_test_router(tracker: Arc<dyn FixAttemptTracker>) -> Router {
        let config = test_config();
        create_api_router(config, tracker).layer(CookieManagerLayer::new())
    }

    /// Create a test user and return the session token.
    fn create_test_session(tracker: &Arc<dyn FixAttemptTracker>) -> String {
        let sqlite = tracker.as_any().downcast_ref::<SqliteTracker>().unwrap();
        let hash = bcrypt::hash("testpass", 4).unwrap(); // cost=4 for fast tests
        let user_id = sqlite.create_user("test@test.com", &hash, "Test", "admin").unwrap();
        sqlite.create_session(user_id, "2099-12-31T23:59:59").unwrap()
    }

    /// Build a GET request with auth cookie.
    fn auth_get(uri: &str, token: &str) -> Request<Body> {
        Request::builder()
            .uri(uri)
            .header("cookie", format!("claudear_session={}", token))
            .body(Body::empty())
            .unwrap()
    }
```

Then update each test to use `create_test_router`, `create_test_session`, and `auth_get`:

```rust
    #[tokio::test]
    async fn test_health_endpoint() {
        let tracker = create_test_tracker();
        let token = create_test_session(&tracker);
        let router = create_test_router(tracker);

        let response = router
            .oneshot(auth_get("/api/health", &token))
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
    }
```

Repeat this pattern for ALL existing tests. Also add a test that verifies unauthenticated requests get 401:

```rust
    #[tokio::test]
    async fn test_unauthenticated_returns_401() {
        let tracker = create_test_tracker();
        let router = create_test_router(tracker);

        let response = router
            .oneshot(
                Request::builder()
                    .uri("/api/stats")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }
```

**Step 2: Run all backend tests**

Run: `cargo test`
Expected: all tests pass

**Step 3: Commit**

```bash
git add src/api/routes.rs
git commit -m "test(auth): update existing API tests to include authentication"
```

---

### Task 9: Write auth-specific backend tests

**Files:**
- Modify: `src/api/auth.rs` (add test module)

**Step 1: Add tests for auth handlers**

Add a `#[cfg(test)] mod tests` block at the bottom of `src/api/auth.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::routes::create_api_router;
    use crate::config::{
        CascadeConfig, ClaudeConfig, Config, DiscordConfig, EmailConfig,
        GitHubAppConfig, GitHubConfig, PushConfig, RegressionConfig, RetryConfig, SmsConfig,
    };
    use crate::storage::SqliteTracker;
    use axum::body::Body;
    use axum::http::Request;
    use http_body_util::BodyExt;
    use tower::ServiceExt;
    use tower_cookies::CookieManagerLayer;

    fn test_config() -> Config {
        Config {
            work_dir: "/tmp/repos".into(),
            known_orgs: vec![],
            auto_discover_paths: vec![],
            poll_interval_ms: 300_000,
            webhook_port: 3100,
            db_path: ":memory:".into(),
            max_issues_per_cycle: 5,
            max_concurrent: 1,
            processing_delay_ms: 5000,
            max_activity_entries: 100,
            ipc_timeout_secs: 30,
            claude_timeout_secs: 21600,
            claude: ClaudeConfig::default(),
            discord: DiscordConfig::default(),
            email: EmailConfig::default(),
            sms: SmsConfig::default(),
            push: PushConfig::default(),
            github: GitHubConfig::default(),
            github_app: GitHubAppConfig::default(),
            retry: RetryConfig::default(),
            linear: None,
            sentry: None,
            regression: RegressionConfig::default(),
            cascade: CascadeConfig::default(),
            users: std::collections::HashMap::new(),
        }
    }

    fn setup() -> (axum::Router, Arc<dyn crate::storage::FixAttemptTracker>) {
        let tracker: Arc<dyn crate::storage::FixAttemptTracker> =
            Arc::new(SqliteTracker::in_memory().unwrap());
        let router = create_api_router(test_config(), tracker.clone())
            .layer(CookieManagerLayer::new());
        (router, tracker)
    }

    fn seed_admin(tracker: &Arc<dyn crate::storage::FixAttemptTracker>) -> String {
        let sqlite = tracker.as_any().downcast_ref::<SqliteTracker>().unwrap();
        let hash = bcrypt::hash("adminpass", 4).unwrap();
        let id = sqlite.create_user("admin@test.com", &hash, "Admin", "admin").unwrap();
        sqlite.create_session(id, "2099-12-31T23:59:59").unwrap()
    }

    #[tokio::test]
    async fn test_login_success() {
        let (router, tracker) = setup();
        let sqlite = tracker.as_any().downcast_ref::<SqliteTracker>().unwrap();
        let hash = bcrypt::hash("secret", 4).unwrap();
        sqlite.create_user("user@test.com", &hash, "User", "viewer").unwrap();

        let response = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/auth/login")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"email":"user@test.com","password":"secret"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let set_cookie = response.headers().get("set-cookie").unwrap().to_str().unwrap();
        assert!(set_cookie.contains("claudear_session="));
    }

    #[tokio::test]
    async fn test_login_wrong_password() {
        let (router, tracker) = setup();
        let sqlite = tracker.as_any().downcast_ref::<SqliteTracker>().unwrap();
        let hash = bcrypt::hash("secret", 4).unwrap();
        sqlite.create_user("user@test.com", &hash, "User", "viewer").unwrap();

        let response = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/auth/login")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"email":"user@test.com","password":"wrong"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn test_login_unknown_email() {
        let (router, _) = setup();

        let response = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/auth/login")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"email":"nobody@test.com","password":"any"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn test_me_authenticated() {
        let (router, tracker) = setup();
        let token = seed_admin(&tracker);

        let response = router
            .oneshot(
                Request::builder()
                    .uri("/api/auth/me")
                    .header("cookie", format!("claudear_session={}", token))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let user: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(user["email"], "admin@test.com");
    }

    #[tokio::test]
    async fn test_me_unauthenticated() {
        let (router, _) = setup();

        let response = router
            .oneshot(
                Request::builder()
                    .uri("/api/auth/me")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn test_create_user_admin_only() {
        let (router, tracker) = setup();
        let token = seed_admin(&tracker);

        let response = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/users")
                    .header("content-type", "application/json")
                    .header("cookie", format!("claudear_session={}", token))
                    .body(Body::from(r#"{"email":"new@test.com","password":"pass","name":"New","role":"viewer"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::CREATED);
    }

    #[tokio::test]
    async fn test_viewer_cannot_create_user() {
        let (router, tracker) = setup();
        let sqlite = tracker.as_any().downcast_ref::<SqliteTracker>().unwrap();
        let hash = bcrypt::hash("viewerpass", 4).unwrap();
        let id = sqlite.create_user("viewer@test.com", &hash, "Viewer", "viewer").unwrap();
        let token = sqlite.create_session(id, "2099-12-31T23:59:59").unwrap();

        let response = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/users")
                    .header("content-type", "application/json")
                    .header("cookie", format!("claudear_session={}", token))
                    .body(Body::from(r#"{"email":"new@test.com","password":"pass","name":"New","role":"viewer"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn test_cannot_delete_self() {
        let (router, tracker) = setup();
        let token = seed_admin(&tracker);
        let sqlite = tracker.as_any().downcast_ref::<SqliteTracker>().unwrap();
        let admin = sqlite.get_user_by_email("admin@test.com").unwrap().unwrap();

        let response = router
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri(&format!("/api/users/{}", admin.id))
                    .header("cookie", format!("claudear_session={}", token))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }
}
```

**Step 2: Run all tests**

Run: `cargo test`
Expected: all tests pass

**Step 3: Commit**

```bash
git add src/api/auth.rs
git commit -m "test(auth): add comprehensive auth endpoint tests"
```

---

### Task 10: Add frontend tests

**Files:**
- Create: `dashboard/test/auth.test.ts`
- Modify: `dashboard/test/api.test.ts`

**Step 1: Create `dashboard/test/auth.test.ts`**

```typescript
import { describe, expect, test, mock, afterEach } from 'bun:test'
import { login, logout, getMe, fetchUsers, createUser, deleteUser } from '../src/lib/api'

function mockFetch(data: unknown, status = 200) {
  globalThis.fetch = mock(() =>
    Promise.resolve({
      ok: status >= 200 && status < 300,
      status,
      json: () => Promise.resolve(data),
    })
  ) as unknown as typeof fetch
}

describe('auth api', () => {
  const originalFetch = globalThis.fetch

  afterEach(() => {
    globalThis.fetch = originalFetch
  })

  test('login sends credentials and returns user', async () => {
    const mockResponse = { user: { id: 1, email: 'admin@test.com', name: 'Admin', role: 'admin' } }
    mockFetch(mockResponse)

    const result = await login('admin@test.com', 'password')
    expect(result.user.email).toBe('admin@test.com')
    expect(fetch).toHaveBeenCalledTimes(1)
  })

  test('login throws on 401', async () => {
    mockFetch(null, 401)
    expect(login('bad@test.com', 'wrong')).rejects.toThrow()
  })

  test('getMe returns current user', async () => {
    mockFetch({ id: 1, email: 'admin@test.com', name: 'Admin', role: 'admin' })
    const user = await getMe()
    expect(user.email).toBe('admin@test.com')
  })

  test('fetchUsers returns user list', async () => {
    mockFetch([
      { id: 1, email: 'a@test.com', name: 'A', role: 'admin', created_at: '', updated_at: '' },
    ])
    const users = await fetchUsers()
    expect(users).toHaveLength(1)
  })

  test('createUser sends user data', async () => {
    mockFetch({ id: 2, email: 'new@test.com', name: 'New', role: 'viewer', created_at: '', updated_at: '' }, 201)
    const user = await createUser({ email: 'new@test.com', password: 'pass', name: 'New', role: 'viewer' })
    expect(user.email).toBe('new@test.com')
  })
})
```

**Step 2: Update existing `dashboard/test/api.test.ts`**

Update the `mockFetch` function to also handle 401 status, and ensure existing tests still pass. The `mockFetch` signature should match what already exists — just verify existing tests still work.

**Step 3: Run frontend tests**

Run: `cd dashboard && bun test`
Expected: all tests pass

**Step 4: Commit**

```bash
git add dashboard/test/auth.test.ts dashboard/test/api.test.ts
git commit -m "test(auth): add frontend auth API tests"
```

---

### Task 11: Final integration verification

**Step 1: Run all backend tests**

Run: `cargo test`
Expected: all pass

**Step 2: Run all frontend tests**

Run: `cd /Users/jakebarnby/Local/claudear/dashboard && bun test`
Expected: all pass

**Step 3: Run cargo clippy**

Run: `cargo clippy`
Expected: no warnings from new code

**Step 4: Typecheck frontend**

Run: `cd /Users/jakebarnby/Local/claudear/dashboard && bun run typecheck`
Expected: no errors

**Step 5: Build frontend**

Run: `cd /Users/jakebarnby/Local/claudear/dashboard && bun run build`
Expected: successful build

**Step 6: Final commit if any cleanup needed**

```bash
git add -A
git commit -m "chore(auth): final cleanup and integration verification"
```
