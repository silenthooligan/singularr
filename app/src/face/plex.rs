//! The Plex sign-in (the plex.tv PIN flow), through `core::outbound`.
//!
//! 1. `POST https://plex.tv/api/v2/pins?strong=true` makes a PIN: an id and a code.
//! 2. The person's browser goes to `https://app.plex.tv/auth#?clientID=..&code=..&forwardUrl=..`,
//!    signs in to Plex there and approves the PIN; Plex sends the browser back to the forward
//!    URL.
//! 3. `GET https://plex.tv/api/v2/pins/{id}` gives the PIN's auth token once approved.
//! 4. `GET https://plex.tv/api/v2/user` with that token names the account: its id and name.
//!
//! The token is used for that one call and dropped: it is never stored or logged. The email
//! address plex.tv sends is not read.

use crate::core::outbound::{
    Body, OutRequest, Outbound, OutboundError, PLEX_PIN_PATH, PLEX_TV_HOST, encode_component,
    expect_ok,
};
use serde::Deserialize;

/// What the sign-in pages call Singularr Requests at Plex.
pub const PRODUCT: &str = "Singularr Requests";

#[derive(Debug, thiserror::Error)]
pub enum PlexError {
    #[error(transparent)]
    Outbound(#[from] OutboundError),
    #[error("plex.tv sent an answer that could not be read: {0}")]
    Decode(String),
}

/// A sign-in PIN.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pin {
    pub id: i64,
    pub code: String,
}

/// The Plex account a sign-in named.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlexAccount {
    pub id: i64,
    pub username: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawPin {
    id: Option<i64>,
    code: Option<String>,
    auth_token: Option<String>,
}

/// The account as plex.tv sends it. `email` is not read.
#[derive(Deserialize)]
struct RawUser {
    id: Option<i64>,
    username: Option<String>,
    title: Option<String>,
}

pub struct PlexSignIn {
    client_id: String,
    out: Outbound,
}

impl PlexSignIn {
    /// `client_id` names this installation at Plex (a stable value, not a secret).
    pub fn new(client_id: String, out: Outbound) -> PlexSignIn {
        PlexSignIn { client_id, out }
    }

    fn headers(&self, req: OutRequest) -> OutRequest {
        req.header("Accept", "application/json")
            .header("X-Plex-Client-Identifier", self.client_id.clone())
            .header("X-Plex-Product", PRODUCT)
            .header("X-Plex-Version", env!("CARGO_PKG_VERSION"))
    }

    fn decode<T: serde::de::DeserializeOwned>(body: &[u8]) -> Result<T, PlexError> {
        serde_json::from_slice(body).map_err(|e| PlexError::Decode(e.to_string()))
    }

    /// A new PIN.
    pub async fn create_pin(&self) -> Result<Pin, PlexError> {
        let url = format!("https://{PLEX_TV_HOST}{PLEX_PIN_PATH}?strong=true");
        let res = self
            .out
            .send(self.headers(OutRequest::post(&url, Body::Form(Vec::new()))))
            .await?;
        expect_ok(&url, &res)?;
        let raw: RawPin = Self::decode(&res.body)?;
        match (raw.id, raw.code) {
            (Some(id), Some(code)) if id > 0 && !code.is_empty() => Ok(Pin { id, code }),
            _ => Err(PlexError::Decode(
                "the PIN has no id or no code".to_string(),
            )),
        }
    }

    /// The auth token of a PIN once the person approved it, else `None`.
    pub async fn pin_token(&self, pin_id: i64) -> Result<Option<String>, PlexError> {
        let url = format!("https://{PLEX_TV_HOST}{PLEX_PIN_PATH}/{pin_id}");
        let res = self.out.send(self.headers(OutRequest::get(&url))).await?;
        if res.status == 404 {
            return Ok(None);
        }
        expect_ok(&url, &res)?;
        let raw: RawPin = Self::decode(&res.body)?;
        Ok(raw.auth_token.filter(|t| !t.trim().is_empty()))
    }

    /// The account a token belongs to. The token is not kept.
    pub async fn account(&self, token: String) -> Result<PlexAccount, PlexError> {
        let url = format!("https://{PLEX_TV_HOST}/api/v2/user");
        let res = self
            .out
            .send(
                self.headers(OutRequest::get(&url))
                    .header("X-Plex-Token", token),
            )
            .await?;
        expect_ok(&url, &res)?;
        let raw: RawUser = Self::decode(&res.body)?;
        let id = raw
            .id
            .filter(|n| *n > 0)
            .ok_or_else(|| PlexError::Decode("the account has no id".to_string()))?;
        let username = raw
            .username
            .or(raw.title)
            .map(|n| n.trim().to_string())
            .filter(|n| !n.is_empty() && !n.contains('@'));
        Ok(PlexAccount { id, username })
    }

    /// Where the browser approves a PIN, coming back to `forward_url`.
    pub fn auth_url(&self, code: &str, forward_url: &str) -> String {
        format!(
            "https://app.plex.tv/auth#?clientID={}&code={}&forwardUrl={}&context%5Bdevice%5D%5Bproduct%5D={}",
            encode_component(&self.client_id),
            encode_component(code),
            encode_component(forward_url),
            encode_component(PRODUCT)
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::outbound::recorded::Recorded;
    use crate::core::outbound::{Allowlist, Method};

    fn plex(recorded: std::sync::Arc<Recorded>) -> PlexSignIn {
        PlexSignIn::new(
            "client-1".to_string(),
            Outbound::with_transport(Allowlist::for_requests_face(), recorded),
        )
    }

    #[tokio::test]
    async fn the_pin_flow_names_the_account_and_drops_the_token() {
        let recorded = Recorded::new();
        recorded.answer(
            Method::Post,
            "https://plex.tv/api/v2/pins?strong=true",
            201,
            r#"{"id": 4242, "code": "abcd1234", "authToken": null}"#,
        );
        recorded.answer(
            Method::Get,
            "https://plex.tv/api/v2/pins/4242",
            200,
            r#"{"id": 4242, "code": "abcd1234", "authToken": "plex-token"}"#,
        );
        recorded.answer(Method::Get, "https://plex.tv/api/v2/user", 200, r#"{"id": 1000007, "uuid": "u", "username": "brightfriend", "email": "friend@example.org"}"#);
        let p = plex(recorded.clone());
        let pin = p.create_pin().await.unwrap();
        assert_eq!(
            pin,
            Pin {
                id: 4242,
                code: "abcd1234".to_string()
            }
        );
        let token = p.pin_token(pin.id).await.unwrap().unwrap();
        let account = p.account(token).await.unwrap();
        assert_eq!(
            account,
            PlexAccount {
                id: 1_000_007,
                username: Some("brightfriend".to_string())
            }
        );
        assert_eq!(
            recorded.methods_seen(),
            vec![Method::Post, Method::Get, Method::Get]
        );
        let url = p.auth_url(&pin.code, "https://requests.example.org/signin/plex");
        assert!(url.starts_with("https://app.plex.tv/auth#?clientID=client-1&code=abcd1234&forwardUrl=https%3A%2F%2Frequests.example.org%2Fsignin%2Fplex"));
    }

    #[tokio::test]
    async fn a_pin_not_yet_approved_has_no_token() {
        let recorded = Recorded::new();
        recorded.answer(
            Method::Get,
            "https://plex.tv/api/v2/pins/7",
            200,
            r#"{"id": 7, "code": "x", "authToken": null}"#,
        );
        recorded.answer(Method::Get, "https://plex.tv/api/v2/pins/8", 404, "{}");
        let p = plex(recorded);
        assert_eq!(p.pin_token(7).await.unwrap(), None);
        assert_eq!(p.pin_token(8).await.unwrap(), None);
    }
}
