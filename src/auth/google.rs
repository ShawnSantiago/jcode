use anyhow::Result;
use serde::{Deserialize, Serialize};

const AUTHORIZE_URL: &str = "https://accounts.google.com/o/oauth2/v2/auth";
const TOKEN_URL: &str = "https://oauth2.googleapis.com/token";
const DEFAULT_PORT: u16 = 8456;

pub const SCOPE_READONLY: &str = "https://www.googleapis.com/auth/gmail.readonly";
pub const SCOPE_COMPOSE: &str = "https://www.googleapis.com/auth/gmail.compose";
pub const SCOPE_SEND: &str = "https://www.googleapis.com/auth/gmail.send";
pub const SCOPE_MODIFY: &str = "https://www.googleapis.com/auth/gmail.modify";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum GmailAccessTier {
    #[serde(rename = "full")]
    Full,
    #[serde(rename = "readonly")]
    ReadOnly,
}

impl GmailAccessTier {
    pub fn scopes(&self) -> Vec<&'static str> {
        match self {
            GmailAccessTier::Full => vec![SCOPE_READONLY, SCOPE_COMPOSE, SCOPE_SEND, SCOPE_MODIFY],
            GmailAccessTier::ReadOnly => vec![SCOPE_READONLY, SCOPE_COMPOSE],
        }
    }

    pub fn can_send(&self) -> bool {
        matches!(self, GmailAccessTier::Full)
    }

    pub fn can_delete(&self) -> bool {
        matches!(self, GmailAccessTier::Full)
    }

    pub fn label(&self) -> &'static str {
        match self {
            GmailAccessTier::Full => "Full Access",
            GmailAccessTier::ReadOnly => "Read & Draft Only",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GoogleCredentials {
    pub client_id: String,
    pub client_secret: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GoogleTokens {
    pub access_token: String,
    pub refresh_token: String,
    pub expires_at: i64,
    pub tier: GmailAccessTier,
    pub email: Option<String>,
}

impl GoogleTokens {
    pub fn is_expired(&self) -> bool {
        let now_ms = chrono::Utc::now().timestamp_millis();
        self.expires_at <= now_ms + 60_000
    }
}

fn credentials_path() -> std::path::PathBuf {
    dirs::home_dir()
        .unwrap_or_default()
        .join(".jcode")
        .join("google_credentials.json")
}

fn tokens_path() -> std::path::PathBuf {
    dirs::home_dir()
        .unwrap_or_default()
        .join(".jcode")
        .join("google_oauth.json")
}

pub fn load_credentials() -> Result<GoogleCredentials> {
    let path = credentials_path();
    let data = std::fs::read_to_string(&path).map_err(|_| {
        anyhow::anyhow!(
            "No Google credentials found at {}.\n\
             To set up Gmail integration:\n\
             1. Go to https://console.cloud.google.com/\n\
             2. Create a project and enable the Gmail API\n\
             3. Create OAuth 2.0 credentials (Desktop application)\n\
             4. Save the JSON to {}",
            path.display(),
            path.display()
        )
    })?;

    if let Ok(creds) = serde_json::from_str::<GoogleCredentials>(&data) {
        return Ok(creds);
    }

    #[derive(Deserialize)]
    struct GCloudFormat {
        installed: Option<GCloudInstalled>,
        web: Option<GCloudInstalled>,
    }
    #[derive(Deserialize)]
    struct GCloudInstalled {
        client_id: String,
        client_secret: String,
    }

    let gcloud: GCloudFormat = serde_json::from_str(&data)?;
    let inner = gcloud
        .installed
        .or(gcloud.web)
        .ok_or_else(|| anyhow::anyhow!("Invalid Google credentials format"))?;

    Ok(GoogleCredentials {
        client_id: inner.client_id,
        client_secret: inner.client_secret,
    })
}

pub fn load_tokens() -> Result<GoogleTokens> {
    let path = tokens_path();
    let data = std::fs::read_to_string(&path)
        .map_err(|_| anyhow::anyhow!("No Google tokens found. Run `jcode login google` first."))?;
    let tokens: GoogleTokens = serde_json::from_str(&data)?;
    Ok(tokens)
}

pub fn save_tokens(tokens: &GoogleTokens) -> Result<()> {
    let path = tokens_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let data = serde_json::to_string_pretty(tokens)?;
    std::fs::write(&path, data)?;
    Ok(())
}

pub fn has_tokens() -> bool {
    tokens_path().exists()
}

pub async fn login(tier: GmailAccessTier) -> Result<GoogleTokens> {
    let creds = load_credentials()?;
    let (verifier, challenge) = super::oauth::generate_pkce_public();
    let state = super::oauth::generate_state_public();

    let scopes = tier.scopes().join(" ");
    let redirect_uri = format!("http://localhost:{}", DEFAULT_PORT);

    let auth_url = format!(
        "{}?response_type=code&client_id={}&redirect_uri={}&scope={}&code_challenge={}&code_challenge_method=S256&state={}&access_type=offline&prompt=consent",
        AUTHORIZE_URL,
        urlencoding::encode(&creds.client_id),
        urlencoding::encode(&redirect_uri),
        urlencoding::encode(&scopes),
        challenge,
        state
    );

    eprintln!("\nOpening browser for Google login...\n");
    let _ = open::that(&auth_url);

    eprintln!("If the browser didn't open, visit:\n{}\n", auth_url);

    let code = super::oauth::wait_for_callback(DEFAULT_PORT, &state)?;

    eprintln!("Exchanging code for tokens...");

    let client = reqwest::Client::new();
    let resp = client
        .post(TOKEN_URL)
        .form(&[
            ("grant_type", "authorization_code"),
            ("client_id", &creds.client_id),
            ("client_secret", &creds.client_secret),
            ("code", &code),
            ("code_verifier", &verifier),
            ("redirect_uri", &redirect_uri),
        ])
        .send()
        .await?;

    if !resp.status().is_success() {
        let text = resp.text().await?;
        anyhow::bail!("Google token exchange failed: {}", text);
    }

    #[derive(Deserialize)]
    struct TokenResponse {
        access_token: String,
        refresh_token: Option<String>,
        expires_in: i64,
    }

    let token_resp: TokenResponse = resp.json().await?;
    let expires_at = chrono::Utc::now().timestamp_millis() + (token_resp.expires_in * 1000);

    let refresh_token = token_resp.refresh_token.ok_or_else(|| {
        anyhow::anyhow!("No refresh token received. Try revoking access at https://myaccount.google.com/permissions and logging in again.")
    })?;

    let email = fetch_email(&token_resp.access_token).await.ok();

    let tokens = GoogleTokens {
        access_token: token_resp.access_token,
        refresh_token,
        expires_at,
        tier,
        email,
    };

    save_tokens(&tokens)?;
    Ok(tokens)
}

pub async fn refresh_tokens(tokens: &GoogleTokens) -> Result<GoogleTokens> {
    let creds = load_credentials()?;
    let client = reqwest::Client::new();

    let resp = client
        .post(TOKEN_URL)
        .form(&[
            ("grant_type", "refresh_token"),
            ("client_id", &creds.client_id),
            ("client_secret", &creds.client_secret),
            ("refresh_token", &tokens.refresh_token),
        ])
        .send()
        .await?;

    if !resp.status().is_success() {
        let text = resp.text().await?;
        anyhow::bail!("Google token refresh failed: {}", text);
    }

    #[derive(Deserialize)]
    struct RefreshResponse {
        access_token: String,
        expires_in: i64,
    }

    let refresh_resp: RefreshResponse = resp.json().await?;
    let expires_at = chrono::Utc::now().timestamp_millis() + (refresh_resp.expires_in * 1000);

    let new_tokens = GoogleTokens {
        access_token: refresh_resp.access_token,
        refresh_token: tokens.refresh_token.clone(),
        expires_at,
        tier: tokens.tier,
        email: tokens.email.clone(),
    };

    save_tokens(&new_tokens)?;
    Ok(new_tokens)
}

pub async fn get_valid_token() -> Result<String> {
    let tokens = load_tokens()?;
    if tokens.is_expired() {
        let new_tokens = refresh_tokens(&tokens).await?;
        Ok(new_tokens.access_token)
    } else {
        Ok(tokens.access_token)
    }
}

async fn fetch_email(access_token: &str) -> Result<String> {
    let client = reqwest::Client::new();
    let resp = client
        .get("https://gmail.googleapis.com/gmail/v1/users/me/profile")
        .bearer_auth(access_token)
        .send()
        .await?;

    if !resp.status().is_success() {
        anyhow::bail!("Failed to fetch Gmail profile");
    }

    #[derive(Deserialize)]
    struct Profile {
        #[serde(rename = "emailAddress")]
        email_address: String,
    }

    let profile: Profile = resp.json().await?;
    Ok(profile.email_address)
}
