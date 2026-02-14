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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::routes::create_api_router;
    use crate::config::{
        CascadeConfig, ClaudeConfig, Config, DiscordConfig, EmailConfig, GitHubAppConfig,
        GitHubConfig, PushConfig, RegressionConfig, RetryConfig, SmsConfig,
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
            known_orgs: vec!["test-org".to_string()],
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

    /// Create router + tracker Arc. Seed functions can downcast the Arc.
    fn create_test_app() -> (
        axum::Router,
        std::sync::Arc<dyn crate::storage::FixAttemptTracker>,
    ) {
        let tracker: std::sync::Arc<dyn crate::storage::FixAttemptTracker> =
            std::sync::Arc::new(SqliteTracker::in_memory().unwrap());
        let router =
            create_api_router(test_config(), tracker.clone()).layer(CookieManagerLayer::new());
        (router, tracker)
    }

    /// Seed an admin user and return (user_id, session_token).
    fn seed_admin(
        tracker: &std::sync::Arc<dyn crate::storage::FixAttemptTracker>,
    ) -> (i64, String) {
        let db = tracker.as_any().downcast_ref::<SqliteTracker>().unwrap();
        let password_hash = bcrypt::hash("password", 4).unwrap();
        let user_id = db
            .create_user("admin@test.com", &password_hash, "Admin User", "admin")
            .unwrap();
        let token = db.create_session(user_id, "2099-12-31 23:59:59").unwrap();
        (user_id, token)
    }

    /// Seed a viewer user and return (user_id, session_token).
    fn seed_viewer(
        tracker: &std::sync::Arc<dyn crate::storage::FixAttemptTracker>,
    ) -> (i64, String) {
        let db = tracker.as_any().downcast_ref::<SqliteTracker>().unwrap();
        let password_hash = bcrypt::hash("password", 4).unwrap();
        let user_id = db
            .create_user("viewer@test.com", &password_hash, "Viewer User", "viewer")
            .unwrap();
        let token = db.create_session(user_id, "2099-12-31 23:59:59").unwrap();
        (user_id, token)
    }

    #[tokio::test]
    async fn test_login_success() {
        let (router, tracker) = create_test_app();

        // Seed a user (not via seed_admin, to test login flow directly)
        let db = tracker.as_any().downcast_ref::<SqliteTracker>().unwrap();
        let password_hash = bcrypt::hash("secret123", 4).unwrap();
        db.create_user("user@test.com", &password_hash, "Test User", "admin")
            .unwrap();

        let response = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/auth/login")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        r#"{"email":"user@test.com","password":"secret123"}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);

        // Check that a set-cookie header is present
        let set_cookie = response.headers().get("set-cookie");
        assert!(set_cookie.is_some(), "Expected set-cookie header");
        let cookie_val = set_cookie.unwrap().to_str().unwrap();
        assert!(
            cookie_val.contains("claudear_session"),
            "Cookie should contain claudear_session"
        );

        // Check response body contains user data
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let body_str = String::from_utf8(body.to_vec()).unwrap();
        assert!(body_str.contains("user@test.com"));
        assert!(body_str.contains("Test User"));
    }

    #[tokio::test]
    async fn test_login_wrong_password() {
        let (router, tracker) = create_test_app();

        let db = tracker.as_any().downcast_ref::<SqliteTracker>().unwrap();
        let password_hash = bcrypt::hash("correct_password", 4).unwrap();
        db.create_user("user@test.com", &password_hash, "Test User", "admin")
            .unwrap();

        let response = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/auth/login")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        r#"{"email":"user@test.com","password":"wrong_password"}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn test_login_unknown_email() {
        let (router, _tracker) = create_test_app();

        let response = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/auth/login")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        r#"{"email":"nobody@test.com","password":"whatever"}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn test_me_authenticated() {
        let (router, tracker) = create_test_app();
        let (_user_id, token) = seed_admin(&tracker);

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
        let body_str = String::from_utf8(body.to_vec()).unwrap();
        assert!(body_str.contains("admin@test.com"));
        assert!(body_str.contains("Admin User"));
        assert!(body_str.contains("admin"));
    }

    #[tokio::test]
    async fn test_me_unauthenticated() {
        let (router, _tracker) = create_test_app();

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
        let (router, tracker) = create_test_app();
        let (_admin_id, token) = seed_admin(&tracker);

        let response = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/users")
                    .header("content-type", "application/json")
                    .header("cookie", format!("claudear_session={}", token))
                    .body(Body::from(
                        r#"{"email":"new@test.com","password":"newpass","name":"New User","role":"viewer"}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::CREATED);

        let body = response.into_body().collect().await.unwrap().to_bytes();
        let body_str = String::from_utf8(body.to_vec()).unwrap();
        assert!(body_str.contains("new@test.com"));
        assert!(body_str.contains("New User"));
        assert!(body_str.contains("viewer"));
    }

    #[tokio::test]
    async fn test_viewer_cannot_create_user() {
        let (router, tracker) = create_test_app();
        let (_viewer_id, token) = seed_viewer(&tracker);

        let response = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/users")
                    .header("content-type", "application/json")
                    .header("cookie", format!("claudear_session={}", token))
                    .body(Body::from(
                        r#"{"email":"new@test.com","password":"newpass","name":"New User","role":"viewer"}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn test_cannot_delete_self() {
        let (router, tracker) = create_test_app();
        let (admin_id, token) = seed_admin(&tracker);

        let response = router
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri(&format!("/api/users/{}", admin_id))
                    .header("cookie", format!("claudear_session={}", token))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }
}
