# Dashboard Auth Design

## Overview

Add session-cookie authentication to the dashboard with full CRUD user management in the API/DB and CLI seeding for initial admin users.

## Decisions

- **Auth mechanism**: Server-side sessions in SQLite with HttpOnly cookies
- **Password hashing**: bcrypt
- **Roles**: Admin (full CRUD on users + all dashboard features) and Viewer (read-only dashboard access)
- **Access scope**: Fully locked down — every route requires authentication
- **Session lifetime**: 7 days, expired sessions cleaned up on login

## Database Schema

Two new tables added to the existing SQLite database:

```sql
CREATE TABLE users (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    email TEXT NOT NULL UNIQUE,
    password_hash TEXT NOT NULL,
    name TEXT NOT NULL,
    role TEXT NOT NULL DEFAULT 'viewer',  -- 'admin' or 'viewer'
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE TABLE sessions (
    id TEXT PRIMARY KEY,              -- random 32-byte hex token
    user_id INTEGER NOT NULL,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    expires_at TEXT NOT NULL,
    FOREIGN KEY (user_id) REFERENCES users(id) ON DELETE CASCADE
);
CREATE INDEX idx_sessions_expires ON sessions(expires_at);
```

## Backend API

### New Dependencies

- `bcrypt` — password hashing
- `tower-cookies` — cookie middleware for Axum
- `uuid` — session token generation

### Auth Endpoints (unauthenticated)

- `POST /api/auth/login` — email + password → session cookie + user info
- `POST /api/auth/logout` — clears session cookie, deletes session from DB
- `GET /api/auth/me` — returns current user (requires valid session)

### User CRUD Endpoints (admin-only)

- `GET /api/users` — list all users
- `GET /api/users/{id}` — get user by ID
- `POST /api/users` — create user (email, password, name, role)
- `PUT /api/users/{id}` — update user (name, email, role, optional password)
- `DELETE /api/users/{id}` — delete user (cascade deletes sessions)

### Middleware

Axum extractor `AuthUser` that:
1. Reads `claudear_session` cookie
2. Looks up session in SQLite, checks expiry
3. Returns user (id, email, name, role) or 401

All existing routes wrapped with auth middleware. Only `/api/auth/login` is unauthenticated.

Admin-only routes use a separate `AdminUser` extractor that checks `role == "admin"`.

## CLI

New subcommand:

```
claudear users seed --email <email> --password <password> [--name "Admin User"]
```

Creates an admin user in the DB. If email already exists, updates the password. For initial setup and Docker deployments.

## Frontend

### New Files

- `src/lib/auth.tsx` — AuthContext provider, `useAuth` hook, login/logout API calls
- `src/pages/login.tsx` — Login page with email/password form
- `src/pages/users.tsx` — User management page (admin only, CRUD table)

### Modified Files

- `App.tsx` — Wrap in AuthProvider, show login page when unauthenticated
- `api.ts` — Add auth + user API functions; handle 401 → redirect to login
- `router.tsx` — No structural changes needed (auth handled at App level)
- `app-shell.tsx` — Add user menu (name, logout) + "Users" nav link for admins

### Auth Flow

1. On load, `GET /api/auth/me`
2. If 401 → show login page
3. On successful login → redirect to dashboard
4. On 401 during any API call → redirect to login
5. Logout → `POST /api/auth/logout` → show login page
