//! Authentication and user management handlers.

use crate::storage::SqliteTracker;
use axum::{
    extract::{FromRequestParts, Path, State},
    http::{request::Parts, StatusCode},
    Json,
};
use serde::{Deserialize, Serialize};
use tower_cookies::{Cookie, Cookies};

use super::routes::ApiState;

const SESSION_COOKIE: &str = "claudear_session";
const SESSION_MAX_AGE_DAYS: i64 = 7;

// ─── Extractors ──────────────────────────────────

/// Authenticated user extracted from session cookie.
#[derive(Debug, Clone, Serialize)]
pub struct AuthUser {
    pub id: i64,
    pub email: String,
    pub name: String,
    pub role: String,
}

impl FromRequestParts<ApiState> for AuthUser {
    type Rejection = StatusCode;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &ApiState,
    ) -> Result<Self, Self::Rejection> {
        // Extract cookies from the request
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

/// Admin user extractor — wraps AuthUser and checks role == "admin".
#[derive(Debug, Clone)]
pub struct AdminUser(pub AuthUser);

impl FromRequestParts<ApiState> for AdminUser {
    type Rejection = StatusCode;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &ApiState,
    ) -> Result<Self, Self::Rejection> {
        let user = AuthUser::from_request_parts(parts, state).await?;
        if user.role != "admin" {
            return Err(StatusCode::FORBIDDEN);
        }
        Ok(AdminUser(user))
    }
}

// ─── Request/Response types ──────────────────────────────────

#[derive(Deserialize)]
pub struct LoginRequest {
    pub email: String,
    pub password: String,
}

#[derive(Serialize)]
pub struct LoginResponse {
    pub user: AuthUserResponse,
}

#[derive(Serialize)]
pub struct AuthUserResponse {
    pub id: i64,
    pub email: String,
    pub name: String,
    pub role: String,
}

impl From<&AuthUser> for AuthUserResponse {
    fn from(u: &AuthUser) -> Self {
        AuthUserResponse {
            id: u.id,
            email: u.email.clone(),
            name: u.name.clone(),
            role: u.role.clone(),
        }
    }
}

#[derive(Deserialize)]
pub struct CreateUserRequest {
    pub email: String,
    pub password: String,
    pub name: String,
    pub role: String,
}

#[derive(Deserialize)]
pub struct UpdateUserRequest {
    pub email: Option<String>,
    pub password: Option<String>,
    pub name: Option<String>,
    pub role: Option<String>,
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

#[derive(Serialize)]
pub struct MessageResponse {
    pub message: String,
}

// ─── Auth handlers ──────────────────────────────────

/// POST /api/auth/login
pub async fn login_handler(
    State(state): State<ApiState>,
    cookies: Cookies,
    Json(body): Json<LoginRequest>,
) -> Result<Json<LoginResponse>, StatusCode> {
    let db = state
        .tracker
        .as_any()
        .downcast_ref::<SqliteTracker>()
        .ok_or(StatusCode::INTERNAL_SERVER_ERROR)?;

    // Look up user by email
    let user = db
        .get_user_by_email(&body.email)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::UNAUTHORIZED)?;

    // Verify password
    let valid = bcrypt::verify(&body.password, &user.password_hash)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    if !valid {
        return Err(StatusCode::UNAUTHORIZED);
    }

    // Create session
    let expires_at = chrono::Utc::now() + chrono::Duration::days(SESSION_MAX_AGE_DAYS);
    let expires_str = expires_at.format("%Y-%m-%d %H:%M:%S").to_string();

    let token = db
        .create_session(user.id, &expires_str)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    // Set cookie
    let mut cookie = Cookie::new(SESSION_COOKIE, token);
    cookie.set_path("/");
    cookie.set_http_only(true);
    cookie.set_same_site(tower_cookies::cookie::SameSite::Lax);
    cookie.set_max_age(tower_cookies::cookie::time::Duration::days(
        SESSION_MAX_AGE_DAYS,
    ));
    cookies.add(cookie);

    Ok(Json(LoginResponse {
        user: AuthUserResponse {
            id: user.id,
            email: user.email,
            name: user.name,
            role: user.role,
        },
    }))
}

/// POST /api/auth/logout
pub async fn logout_handler(
    State(state): State<ApiState>,
    cookies: Cookies,
) -> Result<Json<MessageResponse>, StatusCode> {
    let db = state
        .tracker
        .as_any()
        .downcast_ref::<SqliteTracker>()
        .ok_or(StatusCode::INTERNAL_SERVER_ERROR)?;

    // Get session token from cookie
    if let Some(cookie) = cookies.get(SESSION_COOKIE) {
        let token = cookie.value().to_string();
        let _ = db.delete_session(&token);
    }

    // Clear cookie
    let mut cookie = Cookie::new(SESSION_COOKIE, "");
    cookie.set_path("/");
    cookie.set_max_age(tower_cookies::cookie::time::Duration::ZERO);
    cookies.remove(cookie);

    Ok(Json(MessageResponse {
        message: "Logged out".to_string(),
    }))
}

/// GET /api/auth/me
pub async fn me_handler(user: AuthUser) -> Json<AuthUserResponse> {
    Json(AuthUserResponse::from(&user))
}

// ─── User CRUD handlers (admin only) ──────────────────────────────────

/// GET /api/users
pub async fn list_users_handler(
    _admin: AdminUser,
    State(state): State<ApiState>,
) -> Result<Json<Vec<UserResponse>>, StatusCode> {
    let db = state
        .tracker
        .as_any()
        .downcast_ref::<SqliteTracker>()
        .ok_or(StatusCode::INTERNAL_SERVER_ERROR)?;

    let users = db
        .list_users()
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let response: Vec<UserResponse> = users
        .into_iter()
        .map(|u| UserResponse {
            id: u.id,
            email: u.email,
            name: u.name,
            role: u.role,
            created_at: u.created_at,
            updated_at: u.updated_at,
        })
        .collect();

    Ok(Json(response))
}

/// GET /api/users/{id}
pub async fn get_user_handler(
    _admin: AdminUser,
    State(state): State<ApiState>,
    Path(id): Path<i64>,
) -> Result<Json<UserResponse>, StatusCode> {
    let db = state
        .tracker
        .as_any()
        .downcast_ref::<SqliteTracker>()
        .ok_or(StatusCode::INTERNAL_SERVER_ERROR)?;

    let user = db
        .get_user_by_id(id)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::NOT_FOUND)?;

    Ok(Json(UserResponse {
        id: user.id,
        email: user.email,
        name: user.name,
        role: user.role,
        created_at: user.created_at,
        updated_at: user.updated_at,
    }))
}

/// POST /api/users
pub async fn create_user_handler(
    _admin: AdminUser,
    State(state): State<ApiState>,
    Json(body): Json<CreateUserRequest>,
) -> Result<(StatusCode, Json<UserResponse>), StatusCode> {
    // Validate role
    if body.role != "admin" && body.role != "viewer" {
        return Err(StatusCode::BAD_REQUEST);
    }

    let db = state
        .tracker
        .as_any()
        .downcast_ref::<SqliteTracker>()
        .ok_or(StatusCode::INTERNAL_SERVER_ERROR)?;

    // Hash password
    let password_hash = bcrypt::hash(&body.password, bcrypt::DEFAULT_COST)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let id = db
        .create_user(&body.email, &password_hash, &body.name, &body.role)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let user = db
        .get_user_by_id(id)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::INTERNAL_SERVER_ERROR)?;

    Ok((
        StatusCode::CREATED,
        Json(UserResponse {
            id: user.id,
            email: user.email,
            name: user.name,
            role: user.role,
            created_at: user.created_at,
            updated_at: user.updated_at,
        }),
    ))
}

/// PUT /api/users/{id}
pub async fn update_user_handler(
    _admin: AdminUser,
    State(state): State<ApiState>,
    Path(id): Path<i64>,
    Json(body): Json<UpdateUserRequest>,
) -> Result<Json<UserResponse>, StatusCode> {
    // Validate role if provided
    if let Some(ref role) = body.role {
        if role != "admin" && role != "viewer" {
            return Err(StatusCode::BAD_REQUEST);
        }
    }

    let db = state
        .tracker
        .as_any()
        .downcast_ref::<SqliteTracker>()
        .ok_or(StatusCode::INTERNAL_SERVER_ERROR)?;

    // Hash password if provided
    let password_hash = match &body.password {
        Some(pw) => Some(
            bcrypt::hash(pw, bcrypt::DEFAULT_COST)
                .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?,
        ),
        None => None,
    };

    let updated = db
        .update_user(
            id,
            body.email.as_deref(),
            password_hash.as_deref(),
            body.name.as_deref(),
            body.role.as_deref(),
        )
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    if !updated {
        return Err(StatusCode::NOT_FOUND);
    }

    let user = db
        .get_user_by_id(id)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::NOT_FOUND)?;

    Ok(Json(UserResponse {
        id: user.id,
        email: user.email,
        name: user.name,
        role: user.role,
        created_at: user.created_at,
        updated_at: user.updated_at,
    }))
}

/// DELETE /api/users/{id}
pub async fn delete_user_handler(
    admin: AdminUser,
    State(state): State<ApiState>,
    Path(id): Path<i64>,
) -> Result<Json<MessageResponse>, StatusCode> {
    // Can't delete yourself
    if admin.0.id == id {
        return Err(StatusCode::BAD_REQUEST);
    }

    let db = state
        .tracker
        .as_any()
        .downcast_ref::<SqliteTracker>()
        .ok_or(StatusCode::INTERNAL_SERVER_ERROR)?;

    // Delete user sessions first
    let _ = db.delete_user_sessions(id);

    let deleted = db
        .delete_user(id)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    if !deleted {
        return Err(StatusCode::NOT_FOUND);
    }

    Ok(Json(MessageResponse {
        message: "User deleted".to_string(),
    }))
}
