//! Codex OAuth sign-in for the management API (authorization code + PKCE).
//!
//! OpenAI only redirects to `localhost:1455`, so the operator pastes the final
//! redirect URL back through `/oauth-callback` instead of a local listener.

use std::{
    collections::HashMap,
    sync::Mutex,
    time::{Duration, Instant},
};

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use url::Url;

use crate::{
    auth::{AuthStore, CODEX_CLIENT_ID, CODEX_TOKEN_URL},
    error::AppError,
};

const AUTH_URL: &str = "https://auth.openai.com/oauth/authorize";
const REDIRECT_URI: &str = "http://localhost:1455/auth/callback";
const SESSION_TTL: Duration = Duration::from_secs(600);

struct Session {
    verifier: String,
    started: Instant,
    error: Option<String>,
}

/// Pending sign-ins keyed by OAuth `state`. A finished sign-in is removed.
#[derive(Default)]
pub struct Sessions(Mutex<HashMap<String, Session>>);

pub enum Status {
    Done,
    Waiting,
    Failed(String),
}

impl Sessions {
    /// Start a sign-in and return `(authorization url, state)`.
    pub fn start(&self) -> Result<(String, String), AppError> {
        let state = random_token(16)?;
        let verifier = random_token(64)?;
        let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
        let url = Url::parse_with_params(
            AUTH_URL,
            [
                ("client_id", CODEX_CLIENT_ID),
                ("response_type", "code"),
                ("redirect_uri", REDIRECT_URI),
                ("scope", "openid email profile offline_access"),
                ("state", &state),
                ("code_challenge", &challenge),
                ("code_challenge_method", "S256"),
                ("prompt", "login"),
                ("id_token_add_organizations", "true"),
                ("codex_cli_simplified_flow", "true"),
            ],
        )
        .expect("static authorization URL");
        let mut sessions = self.0.lock().unwrap();
        sessions.retain(|_, session| session.started.elapsed() < SESSION_TTL);
        sessions.insert(
            state.clone(),
            Session {
                verifier,
                started: Instant::now(),
                error: None,
            },
        );
        Ok((url.into(), state))
    }

    pub fn status(&self, state: &str) -> Status {
        match self.0.lock().unwrap().get(state) {
            None => Status::Done,
            Some(Session {
                error: Some(error), ..
            }) => Status::Failed(error.clone()),
            Some(_) => Status::Waiting,
        }
    }

    fn verifier(&self, state: &str) -> Option<String> {
        let sessions = self.0.lock().unwrap();
        let session = sessions.get(state)?;
        (session.started.elapsed() < SESSION_TTL && session.error.is_none())
            .then(|| session.verifier.clone())
    }

    fn finish(&self, state: &str, result: &Result<String, AppError>) {
        let mut sessions = self.0.lock().unwrap();
        match result {
            Ok(_) => {
                sessions.remove(state);
            }
            Err(error) => {
                if let Some(session) = sessions.get_mut(state) {
                    session.error = Some(error.message().to_owned());
                }
            }
        }
    }

    /// Exchange the authorization code and store the credential. Returns the
    /// new auth file name.
    pub async fn complete(
        &self,
        client: &reqwest::Client,
        auth: &AuthStore,
        state: &str,
        code: &str,
    ) -> Result<String, AppError> {
        let verifier = self
            .verifier(state)
            .ok_or_else(|| AppError::not_found("unknown or expired state"))?;
        let result = exchange(client, auth, code, &verifier).await;
        self.finish(state, &result);
        result
    }

    pub fn fail(&self, state: &str, message: &str) {
        self.finish(state, &Err(AppError::bad_request(message)));
    }
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    #[serde(default)]
    refresh_token: String,
    #[serde(default)]
    id_token: String,
}

async fn exchange(
    client: &reqwest::Client,
    auth: &AuthStore,
    code: &str,
    verifier: &str,
) -> Result<String, AppError> {
    let response = client
        .post(CODEX_TOKEN_URL)
        .header("accept", "application/json")
        .form(&[
            ("grant_type", "authorization_code"),
            ("client_id", CODEX_CLIENT_ID),
            ("code", code),
            ("redirect_uri", REDIRECT_URI),
            ("code_verifier", verifier),
        ])
        .send()
        .await
        .map_err(|_| AppError::bad_gateway("Codex token exchange request failed"))?;
    if !response.status().is_success() {
        return Err(AppError::bad_gateway(format!(
            "Codex token exchange returned HTTP {}",
            response.status()
        )));
    }
    let token: TokenResponse = response
        .json()
        .await
        .map_err(|_| AppError::bad_gateway("invalid Codex token response"))?;
    if token.access_token.is_empty() {
        return Err(AppError::bad_gateway(
            "Codex token response omitted access_token",
        ));
    }

    let claims = id_token_claims(&token.id_token);
    let email = claims
        .get("email")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let account_id = claims
        .pointer("/https:~1~1api.openai.com~1auth/chatgpt_account_id")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let label: String = if email.is_empty() { account_id } else { email }
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '@' | '.' | '-' | '_' | '+'))
        .collect();
    let name = if label.is_empty() {
        format!("codex-{}.json", random_token(4)?)
    } else {
        format!("codex-{label}.json")
    };
    let file = json!({
        "type": "codex",
        "id_token": token.id_token,
        "access_token": token.access_token,
        "refresh_token": token.refresh_token,
        "account_id": account_id,
        "email": email,
    });
    auth.upload(&name, file.to_string().as_bytes()).await?;
    Ok(name)
}

/// Unverified payload of the ID token; only used to label the credential.
fn id_token_claims(id_token: &str) -> Value {
    id_token
        .split('.')
        .nth(1)
        .and_then(|payload| URL_SAFE_NO_PAD.decode(payload).ok())
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or(Value::Null)
}

fn random_token(bytes: usize) -> Result<String, AppError> {
    let mut buffer = vec![0u8; bytes];
    getrandom::fill(&mut buffer)
        .map_err(|_| AppError::bad_gateway("system randomness is unavailable"))?;
    Ok(URL_SAFE_NO_PAD.encode(buffer))
}

#[cfg(test)]
mod tests {
    use super::{Sessions, Status, id_token_claims};

    #[test]
    fn start_builds_a_pkce_url_and_tracks_the_state() {
        let sessions = Sessions::default();
        let (url, state) = sessions.start().unwrap();
        assert!(url.starts_with("https://auth.openai.com/oauth/authorize?"));
        assert!(url.contains("code_challenge_method=S256"));
        assert!(url.contains(&format!("state={state}")));
        assert!(matches!(sessions.status(&state), Status::Waiting));
        sessions.fail(&state, "access_denied");
        assert!(
            matches!(sessions.status(&state), Status::Failed(message) if message == "access_denied")
        );
    }

    #[test]
    fn reads_claims_from_an_id_token() {
        // {"email":"a@b.c","https://api.openai.com/auth":{"chatgpt_account_id":"acct"}}
        let token = "x.eyJlbWFpbCI6ImFAYi5jIiwiaHR0cHM6Ly9hcGkub3BlbmFpLmNvbS9hdXRoIjp7ImNoYXRncHRfYWNjb3VudF9pZCI6ImFjY3QifX0.y";
        let claims = id_token_claims(token);
        assert_eq!(claims["email"], "a@b.c");
        assert_eq!(
            claims
                .pointer("/https:~1~1api.openai.com~1auth/chatgpt_account_id")
                .unwrap(),
            "acct"
        );
    }
}
