//! Authentication and user management handlers.

use crate::storage::SqliteTracker;
use axum::{
    extract::{FromRequestParts, Path, State},
    http::{request::Parts, StatusCode},
    Json,
};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Instant;
use tower_cookies::{Cookie, Cookies};

use super::routes::ApiState;

const SESSION_COOKIE: &str = "claudear_session";
const SESSION_MAX_AGE_DAYS: i64 = 7;

/// Maximum number of login attempts per email within the rate limit window.
const LOGIN_RATE_LIMIT_MAX_ATTEMPTS: usize = 10;

/// Duration of the rate limit window in seconds.
const LOGIN_RATE_LIMIT_WINDOW_SECS: u64 = 300; // 5 minutes

/// In-memory rate limiter for login attempts, keyed by email address.
/// This protects against brute force attacks on specific accounts and
/// mitigates CPU exhaustion from repeated bcrypt verification.
static LOGIN_RATE_LIMITER: std::sync::LazyLock<Mutex<HashMap<String, Vec<Instant>>>> =
    std::sync::LazyLock::new(|| Mutex::new(HashMap::new()));

/// Check if a login attempt is allowed for the given key (email).
/// Returns true if the attempt is within rate limits, false if it should be rejected.
fn check_login_rate_limit(key: &str) -> bool {
    let mut limiter = match LOGIN_RATE_LIMITER.lock() {
        Ok(l) => l,
        Err(poisoned) => {
            tracing::warn!("Login rate limiter mutex was poisoned, recovering");
            poisoned.into_inner()
        }
    };

    let now = Instant::now();
    let window = std::time::Duration::from_secs(LOGIN_RATE_LIMIT_WINDOW_SECS);

    let attempts = limiter.entry(key.to_string()).or_default();

    // Remove attempts outside the window
    attempts.retain(|t| now.duration_since(*t) < window);

    if attempts.len() >= LOGIN_RATE_LIMIT_MAX_ATTEMPTS {
        return false;
    }

    attempts.push(now);
    true
}

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
    // Rate limit login attempts by email to prevent brute force and bcrypt CPU exhaustion
    if !check_login_rate_limit(&body.email) {
        tracing::warn!(email = %body.email, "Login rate limit exceeded");
        return Err(StatusCode::TOO_MANY_REQUESTS);
    }

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
    cookie.set_secure(true);
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

    // Validate email
    if body.email.trim().is_empty() || !body.email.contains('@') {
        return Err(StatusCode::BAD_REQUEST);
    }

    // Validate password (minimum 8 characters)
    if body.password.len() < 8 {
        return Err(StatusCode::BAD_REQUEST);
    }

    // Validate name
    if body.name.trim().is_empty() {
        return Err(StatusCode::BAD_REQUEST);
    }

    let db = state
        .tracker
        .as_any()
        .downcast_ref::<SqliteTracker>()
        .ok_or(StatusCode::INTERNAL_SERVER_ERROR)?;

    // Check for duplicate email
    if db
        .get_user_by_email(&body.email)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .is_some()
    {
        return Err(StatusCode::CONFLICT);
    }

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
        AskConfig, CascadeConfig, ClaudeConfig, CodeIndexConfig, Config, DiscordConfig,
        EmailConfig, GitHubAppConfig, GitHubConfig, LearningConfig, PrioritisationConfig,
        PushConfig, RegressionConfig, RetryConfig, SlackConfig, SmsConfig,
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
            slack: SlackConfig::default(),
            email: EmailConfig::default(),
            sms: SmsConfig::default(),
            push: PushConfig::default(),
            ask: AskConfig::default(),
            github: GitHubConfig::default(),
            github_app: GitHubAppConfig::default(),
            retry: RetryConfig::default(),
            linear: None,
            sentry: None,
            jira: None,
            gitlab: None,
            regression: RegressionConfig::default(),
            cascade: CascadeConfig::default(),
            users: std::collections::HashMap::new(),
            learning: LearningConfig::default(),
            prioritisation: PrioritisationConfig::default(),
            code_index: CodeIndexConfig::default(),
        }
    }

    /// Create router + tracker Arc. Seed functions can downcast the Arc.
    fn create_test_app() -> (
        axum::Router,
        std::sync::Arc<dyn crate::storage::FixAttemptTracker>,
    ) {
        let tracker: std::sync::Arc<dyn crate::storage::FixAttemptTracker> =
            std::sync::Arc::new(SqliteTracker::in_memory().unwrap());
        let indexing_rx = tracker.subscribe_indexing_progress();
        let router = create_api_router(
            test_config(),
            tracker.clone(),
            std::path::PathBuf::from("claudear.toml"),
            indexing_rx,
        )
        .layer(CookieManagerLayer::new());
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
                        r#"{"email":"new@test.com","password":"newpass1!","name":"New User","role":"viewer"}"#,
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
                        r#"{"email":"new@test.com","password":"newpass1!","name":"New User","role":"viewer"}"#,
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
                    .uri(format!("/api/users/{}", admin_id))
                    .header("cookie", format!("claudear_session={}", token))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    // ─── Parameterized seed helpers for test isolation ─────────────────

    /// Seed an admin user with a custom email and return (user_id, session_token).
    fn seed_admin_with_email(
        tracker: &std::sync::Arc<dyn crate::storage::FixAttemptTracker>,
        email: &str,
    ) -> (i64, String) {
        let db = tracker.as_any().downcast_ref::<SqliteTracker>().unwrap();
        let password_hash = bcrypt::hash("password", 4).unwrap();
        let user_id = db
            .create_user(email, &password_hash, "Admin User", "admin")
            .unwrap();
        let token = db.create_session(user_id, "2099-12-31 23:59:59").unwrap();
        (user_id, token)
    }

    // ─── Rate limiting tests ──────────────────────────────────────────

    #[test]
    fn test_rate_limit_rejects_11th_attempt() {
        // Use a unique email to avoid interference with other tests
        let key = "ratelimit-11th@unique-test.com";
        // First 10 attempts should succeed
        for i in 0..LOGIN_RATE_LIMIT_MAX_ATTEMPTS {
            assert!(
                check_login_rate_limit(key),
                "Attempt {} should be allowed",
                i + 1
            );
        }
        // 11th attempt should be rejected
        assert!(
            !check_login_rate_limit(key),
            "11th attempt should be rejected"
        );
    }

    #[test]
    fn test_rate_limit_independent_keys() {
        let key_a = "ratelimit-indep-a@unique-test.com";
        let key_b = "ratelimit-indep-b@unique-test.com";

        // Exhaust rate limit for key_a
        for _ in 0..LOGIN_RATE_LIMIT_MAX_ATTEMPTS {
            check_login_rate_limit(key_a);
        }
        assert!(
            !check_login_rate_limit(key_a),
            "key_a should be rate-limited"
        );

        // key_b should still be allowed
        assert!(
            check_login_rate_limit(key_b),
            "key_b should NOT be rate-limited"
        );
    }

    // ─── create_user_handler validation tests ─────────────────────────

    #[tokio::test]
    async fn test_create_user_invalid_role() {
        let (router, tracker) = create_test_app();
        let (_admin_id, token) = seed_admin_with_email(&tracker, "admin-create-role@test.com");

        let response = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/users")
                    .header("content-type", "application/json")
                    .header("cookie", format!("claudear_session={}", token))
                    .body(Body::from(
                        r#"{"email":"x@test.com","password":"longpassword","name":"X","role":"superuser"}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn test_create_user_empty_email() {
        let (router, tracker) = create_test_app();
        let (_admin_id, token) = seed_admin_with_email(&tracker, "admin-create-email@test.com");

        let response = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/users")
                    .header("content-type", "application/json")
                    .header("cookie", format!("claudear_session={}", token))
                    .body(Body::from(
                        r#"{"email":"","password":"longpassword","name":"X","role":"viewer"}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn test_create_user_email_without_at() {
        let (router, tracker) = create_test_app();
        let (_admin_id, token) = seed_admin_with_email(&tracker, "admin-create-noat@test.com");

        let response = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/users")
                    .header("content-type", "application/json")
                    .header("cookie", format!("claudear_session={}", token))
                    .body(Body::from(
                        r#"{"email":"bademail","password":"longpassword","name":"X","role":"viewer"}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn test_create_user_password_too_short() {
        let (router, tracker) = create_test_app();
        let (_admin_id, token) = seed_admin_with_email(&tracker, "admin-create-pw@test.com");

        let response = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/users")
                    .header("content-type", "application/json")
                    .header("cookie", format!("claudear_session={}", token))
                    .body(Body::from(
                        r#"{"email":"x@test.com","password":"short","name":"X","role":"viewer"}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn test_create_user_empty_name() {
        let (router, tracker) = create_test_app();
        let (_admin_id, token) = seed_admin_with_email(&tracker, "admin-create-name@test.com");

        let response = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/users")
                    .header("content-type", "application/json")
                    .header("cookie", format!("claudear_session={}", token))
                    .body(Body::from(
                        r#"{"email":"x@test.com","password":"longpassword","name":"","role":"viewer"}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn test_create_user_duplicate_email() {
        let (router, tracker) = create_test_app();
        let (_admin_id, token) = seed_admin_with_email(&tracker, "admin-create-dup@test.com");

        // Create a user first
        let db = tracker.as_any().downcast_ref::<SqliteTracker>().unwrap();
        let hash = bcrypt::hash("password", 4).unwrap();
        db.create_user("dup@test.com", &hash, "Dup User", "viewer")
            .unwrap();

        // Try to create another user with the same email
        let response = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/users")
                    .header("content-type", "application/json")
                    .header("cookie", format!("claudear_session={}", token))
                    .body(Body::from(
                        r#"{"email":"dup@test.com","password":"longpassword","name":"Another","role":"viewer"}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::CONFLICT);
    }

    // ─── update_user_handler validation tests ─────────────────────────

    #[tokio::test]
    async fn test_update_user_invalid_role() {
        let (router, tracker) = create_test_app();
        let (_admin_id, token) = seed_admin_with_email(&tracker, "admin-update-role@test.com");

        // Create a target user to update
        let db = tracker.as_any().downcast_ref::<SqliteTracker>().unwrap();
        let hash = bcrypt::hash("password", 4).unwrap();
        let target_id = db
            .create_user("target-update@test.com", &hash, "Target", "viewer")
            .unwrap();

        let response = router
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri(format!("/api/users/{}", target_id))
                    .header("content-type", "application/json")
                    .header("cookie", format!("claudear_session={}", token))
                    .body(Body::from(r#"{"role":"superuser"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn test_update_user_not_found() {
        let (router, tracker) = create_test_app();
        let (_admin_id, token) = seed_admin_with_email(&tracker, "admin-update-nf@test.com");

        let response = router
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri("/api/users/99999")
                    .header("content-type", "application/json")
                    .header("cookie", format!("claudear_session={}", token))
                    .body(Body::from(r#"{"name":"Updated"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    // ─── delete_user_handler tests ────────────────────────────────────

    #[tokio::test]
    async fn test_delete_user_success() {
        let (router, tracker) = create_test_app();
        let (_admin_id, token) = seed_admin_with_email(&tracker, "admin-del-ok@test.com");

        // Create a target user to delete
        let db = tracker.as_any().downcast_ref::<SqliteTracker>().unwrap();
        let hash = bcrypt::hash("password", 4).unwrap();
        let target_id = db
            .create_user("target-del@test.com", &hash, "Target", "viewer")
            .unwrap();

        let response = router
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri(format!("/api/users/{}", target_id))
                    .header("cookie", format!("claudear_session={}", token))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);

        let body = response.into_body().collect().await.unwrap().to_bytes();
        let body_str = String::from_utf8(body.to_vec()).unwrap();
        assert!(body_str.contains("User deleted"));
    }

    #[tokio::test]
    async fn test_delete_user_not_found() {
        let (router, tracker) = create_test_app();
        let (_admin_id, token) = seed_admin_with_email(&tracker, "admin-del-nf@test.com");

        let response = router
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri("/api/users/99999")
                    .header("cookie", format!("claudear_session={}", token))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    // ─── list_users_handler tests ─────────────────────────────────────

    #[tokio::test]
    async fn test_list_users_as_admin() {
        let (router, tracker) = create_test_app();
        let (_admin_id, token) = seed_admin_with_email(&tracker, "admin-list@test.com");

        // Create an additional user
        let db = tracker.as_any().downcast_ref::<SqliteTracker>().unwrap();
        let hash = bcrypt::hash("password", 4).unwrap();
        db.create_user("extra-list@test.com", &hash, "Extra User", "viewer")
            .unwrap();

        let response = router
            .oneshot(
                Request::builder()
                    .uri("/api/users")
                    .header("cookie", format!("claudear_session={}", token))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);

        let body = response.into_body().collect().await.unwrap().to_bytes();
        let body_str = String::from_utf8(body.to_vec()).unwrap();
        // Should contain both users
        assert!(body_str.contains("admin-list@test.com"));
        assert!(body_str.contains("extra-list@test.com"));

        // Verify the response parses as a JSON array
        let parsed: Vec<serde_json::Value> = serde_json::from_str(&body_str).unwrap();
        assert_eq!(parsed.len(), 2);
    }

    // ─── get_user_handler tests ───────────────────────────────────────

    #[tokio::test]
    async fn test_get_user_by_id() {
        let (router, tracker) = create_test_app();
        let (_admin_id, token) = seed_admin_with_email(&tracker, "admin-getuser@test.com");

        // Create a target user
        let db = tracker.as_any().downcast_ref::<SqliteTracker>().unwrap();
        let hash = bcrypt::hash("password", 4).unwrap();
        let target_id = db
            .create_user("target-get@test.com", &hash, "Target Get", "viewer")
            .unwrap();

        let response = router
            .oneshot(
                Request::builder()
                    .uri(format!("/api/users/{}", target_id))
                    .header("cookie", format!("claudear_session={}", token))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);

        let body = response.into_body().collect().await.unwrap().to_bytes();
        let body_str = String::from_utf8(body.to_vec()).unwrap();
        assert!(body_str.contains("target-get@test.com"));
        assert!(body_str.contains("Target Get"));
        assert!(body_str.contains("viewer"));
    }

    #[tokio::test]
    async fn test_get_user_not_found() {
        let (router, tracker) = create_test_app();
        let (_admin_id, token) = seed_admin_with_email(&tracker, "admin-getuser-nf@test.com");

        let response = router
            .oneshot(
                Request::builder()
                    .uri("/api/users/99999")
                    .header("cookie", format!("claudear_session={}", token))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    // ─── logout_handler tests ─────────────────────────────────────────

    #[tokio::test]
    async fn test_logout_clears_session_cookie() {
        let (router, tracker) = create_test_app();
        let (_admin_id, token) = seed_admin_with_email(&tracker, "admin-logout@test.com");

        let response = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/auth/logout")
                    .header("cookie", format!("claudear_session={}", token))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);

        // Verify response body
        let body_bytes = response.into_body().collect().await.unwrap().to_bytes();
        let body_str = String::from_utf8(body_bytes.to_vec()).unwrap();
        assert!(body_str.contains("Logged out"));
    }

    // ─── AuthUserResponse conversion test ─────────────────────────────

    #[test]
    fn test_auth_user_response_from_auth_user() {
        let user = AuthUser {
            id: 42,
            email: "convert@test.com".to_string(),
            name: "Convert User".to_string(),
            role: "admin".to_string(),
        };

        let response = AuthUserResponse::from(&user);

        assert_eq!(response.id, 42);
        assert_eq!(response.email, "convert@test.com");
        assert_eq!(response.name, "Convert User");
        assert_eq!(response.role, "admin");
    }
}
