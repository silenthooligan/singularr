//! The way out for mail. Every SMTP connection Singularr opens is opened here, by
//! [`Smtp::send`], to the server an email channel names and to no other host: no MX lookup,
//! no relay of its own choosing, no redirect.
//!
//! - The connection is plain, upgraded with STARTTLS, or TLS from the first byte, as the
//!   channel says. A server that does not offer STARTTLS when it is asked for gets nothing:
//!   the connection is never used in the clear instead.
//! - A certificate is checked against the roots the program ships and the ones the system
//!   trusts.
//! - Signing in is AUTH PLAIN, or AUTH LOGIN where that is all the server offers.
//! - Every send is logged with host, port and duration. Neither the sign-in nor the message
//!   is logged.
//! - What sends is behind [`Mailer`], so a test hands its message to [`recorded::Recorded`]
//!   and nothing leaves.

use super::outbound::TIMED_OUT;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use std::fmt;
use std::sync::{Arc, LazyLock};
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;

/// What this client calls itself in EHLO: an address literal, which every server takes.
const CLIENT_NAME: &str = "[127.0.0.1]";
/// The longest line of a server's answer that is read.
const MAX_LINE: u64 = 4096;
/// The most lines one answer may have.
const MAX_LINES: usize = 64;

/// How the connection to a mail server is protected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Security {
    /// Not at all.
    Plain,
    /// A plain connection, upgraded with STARTTLS before anything else is said.
    StartTls,
    /// TLS from the first byte.
    Tls,
}

impl Security {
    /// `none`, `starttls` or `tls`, in any case. `None` for any other word.
    pub fn parse(text: &str) -> Option<Security> {
        match text.trim().to_ascii_lowercase().as_str() {
            "none" => Some(Security::Plain),
            "starttls" => Some(Security::StartTls),
            "tls" => Some(Security::Tls),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Security::Plain => "none",
            Security::StartTls => "starttls",
            Security::Tls => "tls",
        }
    }
}

/// The mail server of one email channel.
#[derive(Clone, PartialEq, Eq)]
pub struct Server {
    pub host: String,
    pub port: u16,
    pub security: Security,
    /// The sign-in name. `None`: the server is not signed in to.
    pub user: Option<String>,
    pub password: Option<String>,
}

/// A server shows whether it has a password, never the password.
impl fmt::Debug for Server {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Server")
            .field("host", &self.host)
            .field("port", &self.port)
            .field("security", &self.security)
            .field("user", &self.user)
            .field("password", &self.password.as_ref().map(|_| "set"))
            .finish()
    }
}

/// Who a message is from or for: `Name <address>`, or the address alone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mailbox {
    pub name: Option<String>,
    pub address: String,
}

/// An address this client sends to: one `@`, a name before it and a domain after it, in
/// ASCII, with nothing a header or a command could be broken with.
fn address_ok(address: &str) -> bool {
    let Some((local, domain)) = address.split_once('@') else {
        return false;
    };
    let plain = |c: char| c.is_ascii_graphic() && !"<>()[]\\,;:\"@".contains(c);
    !local.is_empty()
        && local.chars().all(plain)
        && !domain.is_empty()
        && !domain.starts_with(['.', '-'])
        && !domain.ends_with(['.', '-'])
        && domain
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-')
}

impl Mailbox {
    /// Read `Name <address>` or `address`. The error says what is wrong, in a sentence.
    pub fn parse(text: &str) -> Result<Mailbox, String> {
        let text = text.trim();
        let (name, address) = match text.rsplit_once('<') {
            Some((name, rest)) => match rest.strip_suffix('>') {
                Some(address) => (name.trim().trim_matches('"').trim(), address.trim()),
                None => return Err(format!("`{text}` is not an address.")),
            },
            None => ("", text),
        };
        if !address_ok(address) {
            return Err(format!("`{address}` is not an address."));
        }
        if name.chars().any(char::is_control) {
            return Err("A name holds no control characters.".to_string());
        }
        Ok(Mailbox {
            name: (!name.is_empty()).then(|| name.to_string()),
            address: address.to_string(),
        })
    }

    /// The domain of the address.
    pub fn domain(&self) -> &str {
        self.address
            .rsplit_once('@')
            .map(|(_, domain)| domain)
            .unwrap_or("localhost")
    }

    /// The mailbox as a header names it.
    fn header(&self) -> String {
        match &self.name {
            None => self.address.clone(),
            Some(name) if name.is_ascii() => format!(
                "\"{}\" <{}>",
                name.replace('\\', "\\\\").replace('"', "\\\""),
                self.address
            ),
            Some(name) => format!("{} <{}>", header_text(name), self.address),
        }
    }
}

/// One message: who it is from and for, its subject, its text, and the same as HTML when
/// there is one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mail {
    pub from: Mailbox,
    pub to: Vec<Mailbox>,
    pub subject: String,
    pub text: String,
    pub html: Option<String>,
}

const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// Base64 (RFC 4648), with padding.
pub fn base64(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b = [
            chunk[0],
            chunk.get(1).copied().unwrap_or(0),
            chunk.get(2).copied().unwrap_or(0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        out.push(ALPHABET[(n >> 18) as usize & 63] as char);
        out.push(ALPHABET[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 {
            ALPHABET[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            ALPHABET[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

/// A body as base64 in lines of 76 characters, each ended as mail ends a line.
fn body(text: &str) -> String {
    let encoded = base64(text.as_bytes());
    let mut out = String::with_capacity(encoded.len() + encoded.len() / 38 + 2);
    for line in encoded.as_bytes().chunks(76) {
        // Base64 is ASCII, so any cut is a whole character.
        out.push_str(std::str::from_utf8(line).unwrap_or_default());
        out.push_str("\r\n");
    }
    out
}

/// The bytes of text one encoded word holds at most: its base64 is 56 characters, so a word
/// after `Subject: ` still ends within 78 characters.
const WORD_BYTES: usize = 42;

/// The text of a header: as it is when it is ASCII, else as encoded words (RFC 2047), a
/// few characters each so that no line grows long. Control characters become spaces, so
/// nothing in it ends the header.
fn header_text(text: &str) -> String {
    let clean: String = text
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let clean = clean.trim();
    if clean.is_ascii() {
        return clean.to_string();
    }
    let word = |part: &str| format!("=?UTF-8?B?{}?=", base64(part.as_bytes()));
    let mut words = Vec::new();
    let mut part = String::new();
    for c in clean.chars() {
        if part.len() + c.len_utf8() > WORD_BYTES {
            words.push(word(&part));
            part.clear();
        }
        part.push(c);
    }
    if !part.is_empty() {
        words.push(word(&part));
    }
    words.join("\r\n ")
}

impl Mail {
    /// The message as it is handed to a server (RFC 5322 with MIME): its headers, then the
    /// text alone, or the text and the HTML as two forms of one message. `token` makes the
    /// message's id and the line between its parts: letters and digits.
    pub fn render(&self, at: DateTime<Utc>, token: &str) -> String {
        let to: Vec<String> = self.to.iter().map(Mailbox::header).collect();
        let mut out = String::new();
        let mut header = |name: &str, value: &str| {
            out.push_str(name);
            out.push_str(": ");
            out.push_str(value);
            out.push_str("\r\n");
        };
        header("From", &self.from.header());
        header("To", &to.join(",\r\n "));
        header("Subject", &header_text(&self.subject));
        header("Date", &at.to_rfc2822());
        header("Message-ID", &format!("<{token}@{}>", self.from.domain()));
        header("MIME-Version", "1.0");
        let part = |kind: &str, text: &str| {
            format!(
                "Content-Type: {kind}; charset=utf-8\r\nContent-Transfer-Encoding: base64\r\n\r\n{}",
                body(text)
            )
        };
        match &self.html {
            None => out.push_str(&part("text/plain", &self.text)),
            Some(html) => {
                let line = format!("mq-{token}");
                out.push_str(&format!(
                    "Content-Type: multipart/alternative; boundary=\"{line}\"\r\n\r\n"
                ));
                for (kind, text) in [("text/plain", &self.text), ("text/html", html)] {
                    out.push_str(&format!("--{line}\r\n{}", part(kind, text)));
                }
                out.push_str(&format!("--{line}--\r\n"));
            }
        }
        out
    }
}

/// Why a message was not handed over.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum MailError {
    /// The server said no, in its own words: `535 5.7.8 Authentication failed`.
    #[error("{0}")]
    Refused(String),
    #[error("{host} did not answer: {message}")]
    Transport { host: String, message: String },
}

/// What hands a message to a mail server. Production is [`Smtp`]; tests record.
#[async_trait]
pub trait Mailer: Send + Sync {
    /// Hand one message to the server. `Ok` holds the server's words of acceptance.
    async fn send(&self, server: &Server, mail: &Mail) -> Result<String, MailError>;
}

/// Why the dialogue with a server ended early.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Failure {
    /// The server's own words.
    Refused(String),
    /// The connection: what went wrong with it.
    Io(String),
}

fn io(e: std::io::Error) -> Failure {
    Failure::Io(match e.kind() {
        std::io::ErrorKind::ConnectionRefused => "connection refused".to_string(),
        std::io::ErrorKind::UnexpectedEof => "the connection closed".to_string(),
        _ => e.to_string(),
    })
}

/// One answer of a server: its code, and its lines without the code.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Reply {
    code: u16,
    lines: Vec<String>,
}

impl Reply {
    /// The answer in one line, as a person reads it: `535 5.7.8 Authentication failed`.
    fn words(&self) -> String {
        let text = self.lines.join(" ");
        let text = text.trim();
        if text.is_empty() {
            self.code.to_string()
        } else {
            format!("{} {text}", self.code)
        }
    }
}

/// What a server says it can do, from its answer to EHLO.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Offers {
    starttls: bool,
    plain: bool,
    login: bool,
}

impl Offers {
    fn read(reply: &Reply) -> Offers {
        let mut offers = Offers::default();
        for line in reply.lines.iter().skip(1) {
            let upper = line.trim().to_ascii_uppercase();
            if upper == "STARTTLS" {
                offers.starttls = true;
            }
            if let Some(ways) = upper
                .strip_prefix("AUTH ")
                .or_else(|| upper.strip_prefix("AUTH="))
            {
                let ways: Vec<&str> = ways.split_whitespace().collect();
                offers.plain |= ways.contains(&"PLAIN");
                offers.login |= ways.contains(&"LOGIN");
            }
        }
        offers
    }
}

/// A connection to a server, read answer by answer.
struct Session<S> {
    io: BufReader<S>,
}

impl<S: AsyncRead + AsyncWrite + Unpin> Session<S> {
    fn new(stream: S) -> Session<S> {
        Session {
            io: BufReader::new(stream),
        }
    }

    /// The stream, without what the server sent ahead of its turn: nothing said before an
    /// upgrade to TLS is taken as said after it.
    fn into_inner(self) -> S {
        self.io.into_inner()
    }

    /// The server's next answer, with every line of it.
    async fn reply(&mut self) -> Result<Reply, Failure> {
        let mut lines = Vec::new();
        loop {
            let mut line = String::new();
            let read = (&mut self.io)
                .take(MAX_LINE)
                .read_line(&mut line)
                .await
                .map_err(io)?;
            if read == 0 {
                return Err(Failure::Io("the connection closed".to_string()));
            }
            if !line.ends_with('\n') || lines.len() >= MAX_LINES {
                return Err(Failure::Io("the answer is not SMTP".to_string()));
            }
            let line = line.trim_end_matches(['\r', '\n']);
            let code = line
                .get(..3)
                .and_then(|c| c.parse::<u16>().ok())
                .ok_or_else(|| Failure::Io("the answer is not SMTP".to_string()))?;
            let more = line.as_bytes().get(3) == Some(&b'-');
            lines.push(line.get(4..).unwrap_or_default().to_string());
            if !more {
                return Ok(Reply { code, lines });
            }
        }
    }

    async fn write(&mut self, bytes: &[u8]) -> Result<(), Failure> {
        self.io.write_all(bytes).await.map_err(io)?;
        self.io.flush().await.map_err(io)
    }

    /// Say one line and read the answer.
    async fn ask(&mut self, line: &str) -> Result<Reply, Failure> {
        self.write(format!("{line}\r\n").as_bytes()).await?;
        self.reply().await
    }

    /// Say one line; any answer but one of `codes` is the server's refusal.
    async fn expect(&mut self, line: &str, codes: &[u16]) -> Result<Reply, Failure> {
        let reply = self.ask(line).await?;
        if codes.contains(&reply.code) {
            Ok(reply)
        } else {
            Err(Failure::Refused(reply.words()))
        }
    }

    /// The server's first words.
    async fn greeting(&mut self) -> Result<(), Failure> {
        let reply = self.reply().await?;
        if reply.code == 220 {
            Ok(())
        } else {
            Err(Failure::Refused(reply.words()))
        }
    }

    /// EHLO, or HELO for a server that knows no better and then offers nothing.
    async fn hello(&mut self) -> Result<Offers, Failure> {
        let reply = self.ask(&format!("EHLO {CLIENT_NAME}")).await?;
        if reply.code == 250 {
            return Ok(Offers::read(&reply));
        }
        self.expect(&format!("HELO {CLIENT_NAME}"), &[250]).await?;
        Ok(Offers::default())
    }

    async fn sign_in(&mut self, offers: Offers, user: &str, password: &str) -> Result<(), Failure> {
        if offers.plain {
            let word = base64(format!("\0{user}\0{password}").as_bytes());
            self.expect(&format!("AUTH PLAIN {word}"), &[235]).await?;
            return Ok(());
        }
        if offers.login {
            self.expect("AUTH LOGIN", &[334]).await?;
            self.expect(&base64(user.as_bytes()), &[334]).await?;
            self.expect(&base64(password.as_bytes()), &[235]).await?;
            return Ok(());
        }
        Err(Failure::Refused(
            "The server offers no sign-in on this connection".to_string(),
        ))
    }

    /// Sign in when the channel has a name to sign in with, then hand the message over.
    /// `Ok` holds the server's words of acceptance.
    async fn submit(
        &mut self,
        offers: Offers,
        server: &Server,
        mail: &Mail,
        message: &str,
    ) -> Result<String, Failure> {
        if let Some(user) = server.user.as_deref() {
            let password = server.password.as_deref().unwrap_or_default();
            self.sign_in(offers, user, password).await?;
        }
        self.expect(&format!("MAIL FROM:<{}>", mail.from.address), &[250])
            .await?;
        for to in &mail.to {
            self.expect(&format!("RCPT TO:<{}>", to.address), &[250, 251])
                .await?;
        }
        self.expect("DATA", &[354]).await?;
        self.write(stuffed(message).as_bytes()).await?;
        let taken = self.reply().await?;
        if taken.code != 250 {
            return Err(Failure::Refused(taken.words()));
        }
        // The message is with the server: what it says to the goodbye changes nothing.
        let _ = self.ask("QUIT").await;
        Ok(taken.words())
    }
}

/// A message as DATA takes it: every line ended with CR LF, a line that starts with a dot
/// given a second one, and the lone dot that ends it.
fn stuffed(message: &str) -> String {
    let mut out = String::with_capacity(message.len() + 8);
    for line in message.split('\n') {
        let line = line.strip_suffix('\r').unwrap_or(line);
        if line.starts_with('.') {
            out.push('.');
        }
        out.push_str(line);
        out.push_str("\r\n");
    }
    // A message that ended with a line end gave one empty line too many.
    if message.ends_with('\n') {
        out.truncate(out.len() - 2);
    }
    out.push_str(".\r\n");
    out
}

/// The whole dialogue on a connection that is as protected as it will be: the greeting, the
/// hello, the sign-in and the message.
async fn converse<S: AsyncRead + AsyncWrite + Unpin>(
    stream: S,
    server: &Server,
    mail: &Mail,
    message: &str,
) -> Result<String, Failure> {
    let mut session = Session::new(stream);
    session.greeting().await?;
    let offers = session.hello().await?;
    session.submit(offers, server, mail, message).await
}

/// The roots a mail server's certificate is checked against: the ones the program ships,
/// and the ones the system trusts (an installation's own authority among them).
static TLS: LazyLock<Result<Arc<rustls::ClientConfig>, String>> = LazyLock::new(|| {
    let mut roots = rustls::RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    for cert in rustls_native_certs::load_native_certs().certs {
        // A certificate of the system's that cannot be read is left out.
        let _ = roots.add(cert);
    }
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let config = rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|e| e.to_string())?
        .with_root_certificates(roots)
        .with_no_client_auth();
    Ok(Arc::new(config))
});

/// TLS over a connection, with the server's certificate checked for `host`.
async fn secure(
    host: &str,
    tcp: TcpStream,
) -> Result<tokio_rustls::client::TlsStream<TcpStream>, Failure> {
    let config = TLS.clone().map_err(Failure::Io)?;
    let name = rustls::pki_types::ServerName::try_from(host.to_string())
        .map_err(|_| Failure::Io(format!("`{host}` is not a host name")))?;
    tokio_rustls::TlsConnector::from(config)
        .connect(name, tcp)
        .await
        .map_err(|e| Failure::Io(format!("TLS: {e}")))
}

/// Connect to the channel's server, protect the connection as the channel says, and hand
/// the message over.
async fn deliver(server: &Server, mail: &Mail) -> Result<String, Failure> {
    let token = uuid::Uuid::new_v4().simple().to_string();
    let message = mail.render(Utc::now(), &token);
    let tcp = TcpStream::connect((server.host.as_str(), server.port))
        .await
        .map_err(io)?;
    match server.security {
        Security::Plain => converse(tcp, server, mail, &message).await,
        Security::Tls => {
            let tls = secure(&server.host, tcp).await?;
            converse(tls, server, mail, &message).await
        }
        Security::StartTls => {
            let mut plain = Session::new(tcp);
            plain.greeting().await?;
            if !plain.hello().await?.starttls {
                return Err(Failure::Refused(
                    "The server does not offer STARTTLS".to_string(),
                ));
            }
            plain.expect("STARTTLS", &[220]).await?;
            let tls = secure(&server.host, plain.into_inner()).await?;
            let mut session = Session::new(tls);
            let offers = session.hello().await?;
            session.submit(offers, server, mail, &message).await
        }
    }
}

/// The one place that opens SMTP connections.
#[derive(Debug, Clone, Copy)]
pub struct Smtp {
    /// The longest one message may take, from the connection to the server's last word.
    timeout: Duration,
}

impl Smtp {
    pub fn new(timeout: Duration) -> Smtp {
        Smtp { timeout }
    }
}

#[async_trait]
impl Mailer for Smtp {
    async fn send(&self, server: &Server, mail: &Mail) -> Result<String, MailError> {
        let started = Instant::now();
        let result = match tokio::time::timeout(self.timeout, deliver(server, mail)).await {
            Ok(result) => result,
            Err(_) => Err(Failure::Io(TIMED_OUT.to_string())),
        };
        let ms = started.elapsed().as_millis() as u64;
        let outcome = match &result {
            Ok(_) => "sent",
            Err(Failure::Refused(_)) => "refused",
            Err(Failure::Io(_)) => "error",
        };
        tracing::info!(
            target: "singularr::outbound",
            method = "SMTP",
            host = %server.host,
            port = server.port,
            status = outcome,
            ms,
            "notify:email"
        );
        metrics::counter!(
            "singularr_outbound_requests_total",
            "target" => "notify:email",
            "status" => outcome
        )
        .increment(1);
        result.map_err(|failure| match failure {
            Failure::Refused(words) => MailError::Refused(words),
            Failure::Io(message) => MailError::Transport {
                host: server.host.clone(),
                message,
            },
        })
    }
}

/// A mailer that keeps what it is handed and sends nothing. For tests in every layer.
pub mod recorded {
    use super::*;
    use std::collections::VecDeque;
    use std::sync::Mutex;

    #[derive(Default)]
    pub struct Recorded {
        sent: Mutex<Vec<(Server, Mail)>>,
        answers: Mutex<VecDeque<Result<String, MailError>>>,
    }

    impl Recorded {
        pub fn new() -> Arc<Recorded> {
            Arc::new(Recorded::default())
        }

        /// The next message is not taken: the server says `words`.
        pub fn refuse(&self, words: &str) {
            self.answers
                .lock()
                .unwrap()
                .push_back(Err(MailError::Refused(words.to_string())));
        }

        /// Every message it was handed, in order, with the server each was for.
        pub fn sent(&self) -> Vec<(Server, Mail)> {
            self.sent.lock().unwrap().clone()
        }
    }

    #[async_trait]
    impl Mailer for Recorded {
        async fn send(&self, server: &Server, mail: &Mail) -> Result<String, MailError> {
            self.sent
                .lock()
                .unwrap()
                .push((server.clone(), mail.clone()));
            self.answers
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or_else(|| Ok("250 2.0.0 Ok: queued".to_string()))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::DuplexStream;

    fn decoded(encoded: &str) -> String {
        let mut bytes = Vec::new();
        let mut held = 0u32;
        let mut bits = 0;
        for c in encoded.bytes().filter(|c| !c.is_ascii_whitespace()) {
            let Some(value) = ALPHABET.iter().position(|a| *a == c) else {
                continue;
            };
            held = (held << 6) | value as u32;
            bits += 6;
            if bits >= 8 {
                bits -= 8;
                bytes.push((held >> bits) as u8);
            }
        }
        String::from_utf8(bytes).unwrap()
    }

    fn server(user: Option<&str>) -> Server {
        Server {
            host: "mail.example.org".to_string(),
            port: 587,
            security: Security::StartTls,
            user: user.map(str::to_string),
            password: user.map(|_| "hunter2-password".to_string()),
        }
    }

    fn mail() -> Mail {
        Mail {
            from: Mailbox::parse("Singularr <singularr@example.org>").unwrap(),
            to: vec![
                Mailbox::parse("owner@example.org").unwrap(),
                Mailbox::parse("Zoë <zoe@example.net>").unwrap(),
            ],
            subject: "File upgraded: Amélie (2001)".to_string(),
            text: "File upgraded\nAmélie (2001)\n.A line that starts with a dot".to_string(),
            html: Some("<p>File upgraded</p><p>Amélie (2001)</p>".to_string()),
        }
    }

    #[test]
    fn base64_is_the_standard_s() {
        for (plain, encoded) in [
            ("", ""),
            ("f", "Zg=="),
            ("fo", "Zm8="),
            ("foo", "Zm9v"),
            ("foob", "Zm9vYg=="),
            ("fooba", "Zm9vYmE="),
            ("foobar", "Zm9vYmFy"),
        ] {
            assert_eq!(base64(plain.as_bytes()), encoded);
            assert_eq!(decoded(encoded), plain);
        }
        assert_eq!(base64(&[0xfb, 0xff, 0xfe]), "+//+");
    }

    #[test]
    fn mailboxes_are_read_and_what_is_no_address_is_refused() {
        let plain = Mailbox::parse(" owner@example.org ").unwrap();
        assert_eq!(
            (plain.name.as_deref(), plain.address.as_str()),
            (None, "owner@example.org")
        );
        assert_eq!(plain.header(), "owner@example.org");
        assert_eq!(plain.domain(), "example.org");
        let named = Mailbox::parse("\"Media, at home\" <media@example.org>").unwrap();
        assert_eq!(named.name.as_deref(), Some("Media, at home"));
        assert_eq!(named.header(), "\"Media, at home\" <media@example.org>");
        let accented = Mailbox::parse("Zoë <zoe@example.net>").unwrap();
        assert_eq!(accented.header(), "=?UTF-8?B?Wm/Dqw==?= <zoe@example.net>");
        for bad in [
            "",
            "owner",
            "owner@",
            "@example.org",
            "a b@example.org",
            "owner@exa mple.org",
            "owner@example.org\r\nBcc: x@example.org",
            "owner@example.org>",
            "Name <owner@example.org",
            "<owner@example.org> trailing",
            "owner@-example.org",
            "zoë@example.org",
            "a@b@example.org",
        ] {
            assert!(Mailbox::parse(bad).is_err(), "{bad:?}");
        }
        assert_eq!(Security::parse(" STARTTLS "), Some(Security::StartTls));
        assert_eq!(Security::parse("none"), Some(Security::Plain));
        assert_eq!(Security::parse("ssl"), None);
        assert_eq!(Security::Tls.as_str(), "tls");
    }

    #[test]
    fn a_message_has_its_headers_a_text_part_and_an_html_part() {
        let at: DateTime<Utc> = "2026-11-23T18:00:00Z".parse().unwrap();
        let message = mail().render(at, "abc123");
        let (headers, parts) = message.split_once("\r\n\r\n").unwrap();
        for line in [
            "From: \"Singularr\" <singularr@example.org>",
            "To: owner@example.org,\r\n =?UTF-8?B?Wm/Dqw==?= <zoe@example.net>",
            "Date: Mon, 23 Nov 2026 18:00:00 +0000",
            "Message-ID: <abc123@example.org>",
            "MIME-Version: 1.0",
            "Content-Type: multipart/alternative; boundary=\"mq-abc123\"",
        ] {
            assert!(headers.contains(line), "{line} is missing from\n{headers}");
        }
        // The subject is no plain ASCII: it travels as an encoded word, and reads back.
        let subject = headers
            .lines()
            .find_map(|l| l.strip_prefix("Subject: =?UTF-8?B?"))
            .and_then(|l| l.strip_suffix("?="))
            .expect("the subject is an encoded word");
        assert_eq!(decoded(subject), "File upgraded: Amélie (2001)");
        let pieces: Vec<&str> = parts.split("--mq-abc123").collect();
        assert_eq!(pieces.len(), 4, "two parts and the closing line");
        assert_eq!(pieces[3], "--\r\n");
        let read = |piece: &str| {
            let (head, encoded) = piece.split_once("\r\n\r\n").unwrap();
            (head.trim().to_string(), decoded(encoded))
        };
        let (head, text) = read(pieces[1]);
        assert!(head.starts_with("Content-Type: text/plain; charset=utf-8"));
        assert!(head.ends_with("Content-Transfer-Encoding: base64"));
        assert_eq!(text, mail().text);
        let (head, html) = read(pieces[2]);
        assert!(head.starts_with("Content-Type: text/html; charset=utf-8"));
        assert_eq!(Some(html), mail().html);
        assert!(message.lines().all(|l| l.len() <= 78), "no long line");
        assert!(message.is_ascii(), "nothing in it needs eight bits");

        // Without HTML the message is the text alone.
        let plain = Mail {
            html: None,
            subject: "A subject\r\nBcc: someone@example.org".to_string(),
            ..mail()
        };
        let message = plain.render(at, "abc123");
        assert!(message.contains("Content-Type: text/plain; charset=utf-8\r\n"));
        assert!(!message.contains("multipart"));
        assert!(
            message.contains("Subject: A subject  Bcc: someone@example.org\r\n"),
            "a line end in a subject ends no header"
        );
        // A long subject that is no ASCII is cut into words that each fit a line.
        let long = Mail {
            subject: "é".repeat(120),
            ..mail()
        };
        assert!(long.render(at, "abc123").lines().all(|l| l.len() <= 78));
    }

    #[test]
    fn a_line_that_starts_with_a_dot_is_given_a_second_one() {
        assert_eq!(stuffed("a\r\n.b\r\n"), "a\r\n..b\r\n.\r\n");
        assert_eq!(stuffed("a\n.\nb"), "a\r\n..\r\nb\r\n.\r\n");
        assert_eq!(stuffed(""), "\r\n.\r\n");
    }

    #[test]
    fn a_server_never_shows_its_password() {
        let shown = format!("{:?}", server(Some("singularr")));
        assert!(shown.contains("mail.example.org") && shown.contains("singularr"));
        assert!(!shown.contains("hunter2"), "{shown}");
    }

    /// A mail server in memory: it answers as the script says and keeps every line it is
    /// sent. `refuse` names the command it says no to, with the words it says.
    async fn serve(stream: DuplexStream, offer: &str, refuse: Option<(&str, &str)>) -> Vec<String> {
        let mut io = BufReader::new(stream);
        let mut seen = Vec::new();
        io.write_all(b"220 mail.example.org ESMTP ready\r\n")
            .await
            .unwrap();
        let mut in_data = false;
        loop {
            let mut line = String::new();
            if io.read_line(&mut line).await.unwrap_or(0) == 0 {
                break;
            }
            let line = line.trim_end_matches(['\r', '\n']).to_string();
            seen.push(line.clone());
            if in_data {
                if line == "." {
                    in_data = false;
                    io.write_all(b"250 2.0.0 Ok: queued as 4F2A\r\n")
                        .await
                        .unwrap();
                }
                continue;
            }
            let verb = line.split([' ', ':']).next().unwrap_or_default();
            let answer = match refuse {
                Some((what, words)) if line.starts_with(what) => format!("{words}\r\n"),
                _ => match verb {
                    "EHLO" => {
                        format!("250-mail.example.org\r\n250-{offer}\r\n250 SIZE 35882577\r\n")
                    }
                    "AUTH" if line == "AUTH LOGIN" => "334 VXNlcm5hbWU6\r\n".to_string(),
                    "AUTH" => "235 2.7.0 Accepted\r\n".to_string(),
                    "MAIL" | "RCPT" => "250 2.1.0 Ok\r\n".to_string(),
                    "DATA" => {
                        in_data = true;
                        "354 End data with <CR><LF>.<CR><LF>\r\n".to_string()
                    }
                    "QUIT" => "221 2.0.0 Bye\r\n".to_string(),
                    // The two lines of AUTH LOGIN: the name, then the password.
                    _ if seen.iter().rev().nth(1).is_some_and(|l| l == "AUTH LOGIN") => {
                        "334 UGFzc3dvcmQ6\r\n".to_string()
                    }
                    _ => "235 2.7.0 Accepted\r\n".to_string(),
                },
            };
            io.write_all(answer.as_bytes()).await.unwrap();
            if verb == "QUIT" {
                break;
            }
        }
        seen
    }

    async fn talk(
        offer: &'static str,
        refuse: Option<(&'static str, &'static str)>,
        server: Server,
    ) -> (Result<String, Failure>, Vec<String>) {
        let (ours, theirs) = tokio::io::duplex(64 * 1024);
        let serving = tokio::spawn(serve(theirs, offer, refuse));
        let at: DateTime<Utc> = "2026-11-23T18:00:00Z".parse().unwrap();
        let message = mail().render(at, "abc123");
        let result = converse(ours, &server, &mail(), &message).await;
        (result, serving.await.unwrap())
    }

    #[tokio::test]
    async fn the_dialogue_signs_in_and_hands_the_message_over() {
        let (result, seen) = talk("AUTH PLAIN LOGIN", None, server(Some("singularr"))).await;
        assert_eq!(result.unwrap(), "250 2.0.0 Ok: queued as 4F2A");
        assert_eq!(seen[0], "EHLO [127.0.0.1]");
        assert_eq!(
            seen[1],
            format!("AUTH PLAIN {}", base64(b"\0singularr\0hunter2-password"))
        );
        assert_eq!(
            &seen[2..6],
            [
                "MAIL FROM:<singularr@example.org>",
                "RCPT TO:<owner@example.org>",
                "RCPT TO:<zoe@example.net>",
                "DATA"
            ]
        );
        assert!(seen.iter().any(|l| l == "Message-ID: <abc123@example.org>"));
        assert_eq!(seen[seen.len() - 2..], [".", "QUIT"]);
        // The text's line that starts with a dot is inside base64: no line of the message
        // but the last is a lone dot.
        assert_eq!(seen.iter().filter(|l| l.as_str() == ".").count(), 1);

        // A server that offers LOGIN alone is signed in to with LOGIN.
        let (result, seen) = talk("AUTH=LOGIN", None, server(Some("singularr"))).await;
        assert!(result.is_ok(), "{result:?}");
        assert_eq!(
            &seen[1..4],
            [
                "AUTH LOGIN".to_string(),
                base64(b"singularr"),
                base64(b"hunter2-password")
            ]
        );
        // Without a name to sign in with nothing is signed in to.
        let (result, seen) = talk("AUTH PLAIN", None, server(None)).await;
        assert!(result.is_ok());
        assert!(seen.iter().all(|l| !l.starts_with("AUTH")));
    }

    #[tokio::test]
    async fn a_server_that_says_no_is_quoted() {
        let refused = |result: Result<String, Failure>| match result {
            Err(Failure::Refused(words)) => words,
            other => panic!("not a refusal: {other:?}"),
        };
        let (result, seen) = talk(
            "AUTH PLAIN",
            Some(("AUTH", "535 5.7.8 Username and Password not accepted")),
            server(Some("singularr")),
        )
        .await;
        assert_eq!(
            refused(result),
            "535 5.7.8 Username and Password not accepted"
        );
        assert!(
            seen.iter().all(|l| !l.starts_with("MAIL")),
            "nothing after the refusal"
        );
        let (result, seen) = talk(
            "AUTH PLAIN",
            Some((
                "RCPT TO:<zoe",
                "550-5.1.1 The address does not exist\r\n550 5.1.1 zoe@example.net",
            )),
            server(Some("singularr")),
        )
        .await;
        assert_eq!(
            refused(result),
            "550 5.1.1 The address does not exist 5.1.1 zoe@example.net"
        );
        assert!(!seen.iter().any(|l| l == "DATA"));
        // A server that offers no sign-in is not sent the password some other way.
        let (result, seen) = talk("PIPELINING", None, server(Some("singularr"))).await;
        assert_eq!(
            refused(result),
            "The server offers no sign-in on this connection"
        );
        assert_eq!(seen, ["EHLO [127.0.0.1]"]);
    }

    #[tokio::test]
    async fn the_recorded_mailer_keeps_what_it_is_handed() {
        let mailer = recorded::Recorded::new();
        mailer.refuse("554 5.7.1 Relay access denied");
        let first = mailer.send(&server(None), &mail()).await;
        assert_eq!(
            first,
            Err(MailError::Refused(
                "554 5.7.1 Relay access denied".to_string()
            ))
        );
        assert!(mailer.send(&server(None), &mail()).await.is_ok());
        let sent = mailer.sent();
        assert_eq!(sent.len(), 2);
        assert_eq!(sent[0].1.subject, "File upgraded: Amélie (2001)");
        assert_eq!(
            MailError::Transport {
                host: "mail.example.org".to_string(),
                message: TIMED_OUT.to_string()
            }
            .to_string(),
            "mail.example.org did not answer: timed out"
        );
    }
}
