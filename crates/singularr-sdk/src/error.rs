//! What the way out says of a call it refused, or of a call that got no usable answer.

/// What a transport says of a call that got no answer in time.
pub const TIMED_OUT: &str = "timed out";

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum OutboundError {
    #[error("`{0}` is not a URL Singularr can call")]
    BadUrl(String),
    #[error(
        "{host} is not on the outbound allowlist; Singularr calls only its configured engines and metadata providers"
    )]
    HostNotAllowed { host: String },
    /// The call is neither a GET nor one of the calls the host's rule lists.
    #[error(
        "{method} {path} on {label} ({host}) is refused: it is not a listed call. An integration is sent GET and the calls its adapter lists, nothing else"
    )]
    NotListed {
        method: &'static str,
        host: String,
        label: String,
        path: String,
    },
    /// The call is a listed act, and the integration's access is `read`.
    #[error(
        "{method} {path} on {label} ({host}) is refused: Singularr may only read {label}, not act on it"
    )]
    NotAllowed {
        method: &'static str,
        host: String,
        label: String,
        path: String,
    },
    #[error("{host} did not answer: {message}")]
    Transport { host: String, message: String },
    #[error("{host} answered {status} for {path}")]
    Status {
        host: String,
        path: String,
        status: u16,
    },
    #[error("the answer could not be read: {0}")]
    Decode(String),
    /// The integration's calls pause after repeated failures. Nothing was sent. The cause is
    /// what the last failure said.
    #[error("{integration} is paused for {seconds} s after repeated failures: {cause}")]
    CircuitOpen {
        integration: String,
        seconds: u64,
        cause: String,
    },
    /// The answer was larger than the profile allows. It was not read further.
    #[error("{host} sent an answer larger than {} MB", .limit >> 20)]
    TooLarge { host: String, limit: usize },
}

/// What a transport says of an answer it stopped reading at the profile's limit.
pub const TOO_LARGE: &str = "answer too large";

/// What a transport says of a call that made no connection.
pub const UNREACHABLE: &str = "connection refused or unreachable";

impl OutboundError {
    /// The call went out and no answer came in time. That is not a refusal: whatever was
    /// asked for may still happen.
    pub fn timed_out(&self) -> bool {
        matches!(self, OutboundError::Transport { message, .. } if message == TIMED_OUT)
    }

    /// Its class in the taxonomy.
    pub fn taxonomy(&self) -> ConnectorError {
        ConnectorError::from(self)
    }
}

/// Every way a call to a service can fail, by what it means for the call. It says whether a read
/// may be sent again. It says whether an act took place.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ConnectorError {
    /// The way out refused the call. Nothing was sent.
    #[error("refused before sending: {0}")]
    Refused(Refusal),
    /// No connection was made. Nothing reached the service.
    #[error("{host} could not be reached: {detail}")]
    Unreachable { host: String, detail: String },
    /// The call went out and no answer came in time. What it asked for may have happened.
    #[error("{host} did not answer in time")]
    Timeout { host: String },
    /// The connection broke after the call went out. It may have happened.
    #[error("{host} broke off: {detail}")]
    Interrupted { host: String, detail: String },
    #[error("{0}")]
    Auth(String),
    #[error("rate limited")]
    RateLimited {
        retry_after: Option<std::time::Duration>,
    },
    #[error("not found")]
    NotFound,
    /// The service said no: a 4xx with its own words.
    #[error("{status}: {detail}")]
    Rejected { status: u16, detail: String },
    #[error("server error {status}")]
    Server { status: u16 },
    #[error("{what} could not be read: {detail}")]
    Decode { what: String, detail: String },
    #[error("paused after repeated failures for {seconds} s")]
    CircuitOpen { seconds: u64 },
    #[error("not offered")]
    NotOffered,
    #[error("{0}")]
    Config(String),
}

/// Why the way out refused a call.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Refusal {
    #[error("not a URL that can be called")]
    BadUrl,
    #[error("{host} is not listed")]
    HostNotListed { host: String },
    #[error("{method} {path} is not a listed call")]
    CallNotListed { method: String, path: String },
    #[error("{method} {path} is an act and the access is read")]
    NotAllowed { method: String, path: String },
    #[error("the answer is larger than {limit} bytes")]
    TooLarge { limit: usize },
}

impl ConnectorError {
    /// The class of a status outside 2xx.
    pub fn from_status(status: u16, detail: &str) -> ConnectorError {
        match status {
            401 | 403 => ConnectorError::Auth(format!("{status}: {detail}")),
            404 => ConnectorError::NotFound,
            429 => ConnectorError::RateLimited { retry_after: None },
            400..=499 => ConnectorError::Rejected {
                status,
                detail: detail.to_string(),
            },
            _ => ConnectorError::Server { status },
        }
    }

    /// A read may be sent again: no connection, a broken one, rate limited, 502 to 504.
    pub fn retry_read(&self) -> bool {
        match self {
            ConnectorError::Unreachable { .. }
            | ConnectorError::Interrupted { .. }
            | ConnectorError::RateLimited { .. } => true,
            ConnectorError::Server { status } => matches!(status, 502..=504),
            _ => false,
        }
    }

    /// An act surely did not take place: it was refused, it reached nothing, or the service
    /// said no.
    pub fn act_settled_no(&self) -> bool {
        matches!(
            self,
            ConnectorError::Refused(_)
                | ConnectorError::Unreachable { .. }
                | ConnectorError::CircuitOpen { .. }
                | ConnectorError::RateLimited { .. }
                | ConnectorError::Auth(_)
                | ConnectorError::NotFound
                | ConnectorError::Rejected { .. }
        )
    }

    /// An act may have taken place: the call went out and no clear answer came. A read
    /// settles it.
    pub fn act_unknown(&self) -> bool {
        matches!(
            self,
            ConnectorError::Timeout { .. }
                | ConnectorError::Interrupted { .. }
                | ConnectorError::Server { .. }
        )
    }

    /// The word stored with a failure: `refused`, `unreachable`, `timeout`, `auth`.
    pub fn class(&self) -> &'static str {
        match self {
            ConnectorError::Refused(_) => "refused",
            ConnectorError::Unreachable { .. } => "unreachable",
            ConnectorError::Timeout { .. } => "timeout",
            ConnectorError::Interrupted { .. } => "interrupted",
            ConnectorError::Auth(_) => "auth",
            ConnectorError::RateLimited { .. } => "rate_limited",
            ConnectorError::NotFound => "not_found",
            ConnectorError::Rejected { .. } => "rejected",
            ConnectorError::Server { .. } => "server",
            ConnectorError::Decode { .. } => "decode",
            ConnectorError::CircuitOpen { .. } => "circuit_open",
            ConnectorError::NotOffered => "not_offered",
            ConnectorError::Config(_) => "config",
        }
    }
}

/// Today's errors in the taxonomy.
impl From<&OutboundError> for ConnectorError {
    fn from(e: &OutboundError) -> ConnectorError {
        match e {
            OutboundError::BadUrl(_) => ConnectorError::Refused(Refusal::BadUrl),
            OutboundError::HostNotAllowed { host } => {
                ConnectorError::Refused(Refusal::HostNotListed { host: host.clone() })
            }
            OutboundError::NotListed { method, path, .. } => {
                ConnectorError::Refused(Refusal::CallNotListed {
                    method: method.to_string(),
                    path: path.clone(),
                })
            }
            OutboundError::NotAllowed { method, path, .. } => {
                ConnectorError::Refused(Refusal::NotAllowed {
                    method: method.to_string(),
                    path: path.clone(),
                })
            }
            OutboundError::Transport { host, message } if message == TIMED_OUT => {
                ConnectorError::Timeout { host: host.clone() }
            }
            OutboundError::Transport { host, message } if message == UNREACHABLE => {
                ConnectorError::Unreachable {
                    host: host.clone(),
                    detail: message.clone(),
                }
            }
            OutboundError::Transport { host, message } => ConnectorError::Interrupted {
                host: host.clone(),
                detail: message.clone(),
            },
            OutboundError::Status { status, path, .. } => {
                ConnectorError::from_status(*status, path)
            }
            OutboundError::Decode(detail) => ConnectorError::Decode {
                what: "the answer".to_string(),
                detail: detail.clone(),
            },
            OutboundError::CircuitOpen { seconds, .. } => {
                ConnectorError::CircuitOpen { seconds: *seconds }
            }
            OutboundError::TooLarge { limit, .. } => {
                ConnectorError::Refused(Refusal::TooLarge { limit: *limit })
            }
        }
    }
}
