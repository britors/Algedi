//! Google Drive adapter: implements `CloudProvider` from
//! algedi-provider-trait against the Drive API v3. See PROMPT-ALGEDI.md
//! sec. 3 (auth) and 5.1 (changes.list polling).

pub mod api;
pub mod auth;
pub mod changes;

pub use api::GDriveApi;
pub use auth::{fetch_account_email, revoke, GDriveAuth, GDriveTokens};

/// Default scope: access limited to files created/opened by Algedi.
pub const SCOPE_DRIVE_FILE: &str = "https://www.googleapis.com/auth/drive.file";
/// Advanced option, needed to sync pre-existing folders not created by the app.
pub const SCOPE_DRIVE_FULL: &str = "https://www.googleapis.com/auth/drive";

pub const AUTH_ENDPOINT: &str = "https://accounts.google.com/o/oauth2/v2/auth";
pub const TOKEN_ENDPOINT: &str = "https://oauth2.googleapis.com/token";

pub struct GDriveProvider {
    pub api: GDriveApi,
}

impl GDriveProvider {
    pub fn new(api: GDriveApi) -> Self {
        Self { api }
    }
}
