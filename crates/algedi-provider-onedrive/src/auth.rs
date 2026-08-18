//! OAuth2 authorization-code + PKCE (S256) flow via loopback redirect,
//! against the Microsoft identity platform. See PROMPT-ALGEDI.md sec. 3.

use oauth2::basic::BasicClient;
use oauth2::reqwest::async_http_client;
use oauth2::{
    AuthUrl, AuthorizationCode, ClientId, CsrfToken, PkceCodeChallenge, RedirectUrl, Scope,
    TokenResponse, TokenUrl,
};
use std::net::TcpListener;
use std::time::Duration;

#[derive(Debug, Clone)]
pub struct OneDriveTokens {
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub expires_in_secs: Option<u64>,
}

pub struct OneDriveAuth {
    client: BasicClient,
    redirect_port: u16,
}

impl OneDriveAuth {
    pub fn new(client_id: String, redirect_port: u16) -> Self {
        // Native-app registrations on Microsoft identity platform have no
        // client secret; PKCE is the only confidentiality mechanism.
        //
        // Uses `localhost`, not `127.0.0.1`: the Microsoft identity
        // platform's "Mobile and desktop applications" client type only
        // special-cases the exact redirect URI `http://localhost` (no
        // port) as matching any loopback port — see docs/oauth-setup.md.
        let redirect_url = format!("http://localhost:{redirect_port}");
        let client = BasicClient::new(
            ClientId::new(client_id),
            None,
            AuthUrl::new(crate::AUTH_ENDPOINT.to_string()).unwrap(),
            Some(TokenUrl::new(crate::TOKEN_ENDPOINT.to_string()).unwrap()),
        )
        .set_redirect_uri(RedirectUrl::new(redirect_url).unwrap());

        Self { client, redirect_port }
    }

    pub fn find_free_port() -> std::io::Result<u16> {
        Ok(TcpListener::bind("127.0.0.1:0")?.local_addr()?.port())
    }

    pub async fn authorize(&self, scopes: &[&str]) -> anyhow::Result<OneDriveTokens> {
        let server = tiny_http::Server::http(("127.0.0.1", self.redirect_port))
            .map_err(|e| anyhow::anyhow!("failed to bind loopback redirect server: {e}"))?;

        let (pkce_challenge, pkce_verifier) = PkceCodeChallenge::new_random_sha256();

        let mut req = self
            .client
            .authorize_url(CsrfToken::new_random)
            .set_pkce_challenge(pkce_challenge);
        for scope in scopes {
            req = req.add_scope(Scope::new(scope.to_string()));
        }
        let (auth_url, csrf_token) = req.url();

        std::process::Command::new("xdg-open")
            .arg(auth_url.as_str())
            .spawn()?;

        let (code, returned_state) = capture_redirect(server, Duration::from_secs(300))?;
        anyhow::ensure!(returned_state == *csrf_token.secret(), "CSRF state mismatch");

        let token = self
            .client
            .exchange_code(AuthorizationCode::new(code))
            .set_pkce_verifier(pkce_verifier)
            .request_async(async_http_client)
            .await?;

        Ok(OneDriveTokens {
            access_token: token.access_token().secret().clone(),
            refresh_token: token.refresh_token().map(|t| t.secret().clone()),
            expires_in_secs: token.expires_in().map(|d| d.as_secs()),
        })
    }

    pub async fn refresh(&self, refresh_token: &str) -> anyhow::Result<OneDriveTokens> {
        let token = self
            .client
            .exchange_refresh_token(&oauth2::RefreshToken::new(refresh_token.to_string()))
            .request_async(async_http_client)
            .await?;

        Ok(OneDriveTokens {
            access_token: token.access_token().secret().clone(),
            refresh_token: token
                .refresh_token()
                .map(|t| t.secret().clone())
                .or_else(|| Some(refresh_token.to_string())),
            expires_in_secs: token.expires_in().map(|d| d.as_secs()),
        })
    }
}

/// Blocks on a single request to the loopback redirect URI, extracts
/// `code`/`state` from the query string, and responds with a short
/// "you can close this tab" page.
fn capture_redirect(server: tiny_http::Server, timeout: Duration) -> anyhow::Result<(String, String)> {
    let request = server
        .recv_timeout(timeout)
        .map_err(|e| anyhow::anyhow!("loopback server error: {e}"))?
        .ok_or_else(|| anyhow::anyhow!("timed out waiting for the OAuth redirect"))?;

    let full_url = format!("http://127.0.0.1{}", request.url());
    let parsed = oauth2::url::Url::parse(&full_url)?;

    let mut code = None;
    let mut state = None;
    for (key, value) in parsed.query_pairs() {
        match key.as_ref() {
            "code" => code = Some(value.into_owned()),
            "state" => state = Some(value.into_owned()),
            _ => {}
        }
    }

    let body = "<html><body>Pode fechar esta aba e voltar ao Algedi.</body></html>";
    let response = tiny_http::Response::from_string(body).with_header(
        "Content-Type: text/html; charset=utf-8"
            .parse::<tiny_http::Header>()
            .unwrap(),
    );
    let _ = request.respond(response);

    Ok((
        code.ok_or_else(|| anyhow::anyhow!("redirect had no 'code' parameter"))?,
        state.ok_or_else(|| anyhow::anyhow!("redirect had no 'state' parameter"))?,
    ))
}

#[derive(Debug, serde::Deserialize)]
struct GraphUser {
    mail: Option<String>,
    #[serde(rename = "userPrincipalName")]
    user_principal_name: Option<String>,
}

/// Fetches the account's email via Microsoft Graph's `/me`, used to label
/// the account (PROMPT-ALGEDI.md sec. 9.1). Personal Microsoft accounts
/// sometimes have no `mail`, so this falls back to `userPrincipalName`.
pub async fn fetch_account_email(access_token: &str) -> anyhow::Result<String> {
    let user: GraphUser = reqwest::Client::new()
        .get("https://graph.microsoft.com/v1.0/me")
        .bearer_auth(access_token)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    user.mail
        .or(user.user_principal_name)
        .ok_or_else(|| anyhow::anyhow!("Microsoft Graph /me returned neither mail nor userPrincipalName"))
}

/// Revokes the account's sign-in sessions with Microsoft, so the account no
/// longer has access even if the local Secret Service entry is somehow not
/// cleaned up (PROMPT-ALGEDI.md checklist: "Remover uma conta revoga o
/// token").
pub async fn revoke(access_token: &str) -> anyhow::Result<()> {
    reqwest::Client::new()
        .post("https://graph.microsoft.com/v1.0/me/revokeSignInSessions")
        .bearer_auth(access_token)
        .send()
        .await?
        .error_for_status()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpStream;

    #[test]
    fn capture_redirect_parses_code_and_state() {
        let port = OneDriveAuth::find_free_port().unwrap();
        let server = tiny_http::Server::http(("127.0.0.1", port)).unwrap();
        let handle = std::thread::spawn(move || capture_redirect(server, Duration::from_secs(5)));

        let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
        stream
            .write_all(
                b"GET /?code=test-code&state=test-state HTTP/1.1\r\n\
                  Host: localhost\r\nConnection: close\r\n\r\n",
            )
            .unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).unwrap();
        assert!(response.starts_with("HTTP/1.1 200"));

        let (code, state) = handle.join().unwrap().unwrap();
        assert_eq!(code, "test-code");
        assert_eq!(state, "test-state");
    }

    #[test]
    fn capture_redirect_times_out_without_a_request() {
        let port = OneDriveAuth::find_free_port().unwrap();
        let server = tiny_http::Server::http(("127.0.0.1", port)).unwrap();
        assert!(capture_redirect(server, Duration::from_millis(100)).is_err());
    }
}
