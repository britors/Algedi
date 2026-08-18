//! Microsoft OneDrive adapter: implements `CloudProvider` from
//! algedi-provider-trait against the Microsoft Graph API. See
//! PROMPT-ALGEDI.md sec. 3 (auth) and 5.1 (/delta polling).

pub mod api;
pub mod auth;
pub mod changes;

pub use api::GraphApi;
pub use auth::{fetch_account_email, revoke, OneDriveAuth, OneDriveTokens};

pub const SCOPE_FILES_READWRITE: &str = "Files.ReadWrite.All";
/// Required to obtain a refresh_token from the Microsoft identity platform.
pub const SCOPE_OFFLINE_ACCESS: &str = "offline_access";

/// "common" tenant: personal + work/school accounts.
pub const AUTH_ENDPOINT: &str = "https://login.microsoftonline.com/common/oauth2/v2.0/authorize";
pub const TOKEN_ENDPOINT: &str = "https://login.microsoftonline.com/common/oauth2/v2.0/token";

pub struct OneDriveProvider {
    pub api: GraphApi,
}

impl OneDriveProvider {
    pub fn new(api: GraphApi) -> Self {
        Self { api }
    }
}
