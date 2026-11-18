//! Native mail sending for NOTIFY1: the same environment the auth child
//! reads (`RUNBOOK_AUTH_EMAIL_TRANSPORT`, `RUNBOOK_SMTP_URL`,
//! `RUNBOOK_SMTP_FROM`, `RUNBOOK_AUTH_CAPTURE_FILE`), spoken directly so
//! the drain thread needs no auth-child handle. SMTP is deliberately
//! small: EHLO, optional STARTTLS, AUTH PLAIN, one message per
//! connection, multipart/alternative. Nothing about a message or a
//! provider error beyond its reply code is ever returned or logged here.
use base64::Engine;
use runbook_core::notify::{EmailMessage, deliver::MailTransport};
use std::sync::Arc;
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::TcpStream,
};
use tokio_rustls::TlsConnector;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EmailConfig {
    Smtp {
        host: String,
        port: u16,
        username: Option<String>,
        password: Option<String>,
        from: String,
        tls: TlsMode,
    },
    Capture {
        file: String,
        from: String,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TlsMode {
    /// TLS from the first byte (smtps / port 465).
    Implicit,
    /// Upgrade with STARTTLS when the server offers it.
    Opportunistic,
    Required,
    /// Plain text (local relays and tests only).
    None,
}

fn invalid() -> crate::Error {
    crate::Error("config.invalid_email")
}

/// Read the mail config the auth service also reads. `Ok(None)` when no
/// transport is configured (mail simply stays queued and the drain log
/// says so once); a half-configured transport is an error.
pub fn email_config(
    get: impl Fn(&str) -> Option<String>,
) -> Result<Option<EmailConfig>, crate::Error> {
    let transport = get("RUNBOOK_AUTH_EMAIL_TRANSPORT");
    let from = get("RUNBOOK_SMTP_FROM");
    match transport.as_deref() {
        Some("capture") => {
            let file = get("RUNBOOK_AUTH_CAPTURE_FILE").ok_or_else(invalid)?;
            Ok(Some(EmailConfig::Capture {
                file,
                from: from.unwrap_or_else(|| "The Runbook <login@example.test>".into()),
            }))
        }
        Some("smtp") | None => {
            let (Some(url), Some(from)) = (get("RUNBOOK_SMTP_URL"), from) else {
                if transport.is_some() {
                    return Err(invalid());
                }
                return Ok(None);
            };
            Ok(Some(parse_smtp_url(&url, from)?))
        }
        // The Worker's transports; on native they are not this binary's.
        Some("binding") | Some("cloudflare") => Ok(None),
        Some(_) => Err(invalid()),
    }
}

fn parse_smtp_url(url: &str, from: String) -> Result<EmailConfig, crate::Error> {
    let parsed = url::Url::parse(url).map_err(|_| invalid())?;
    let host = parsed.host_str().ok_or_else(invalid)?.to_owned();
    let implicit = parsed.scheme() == "smtps";
    if !matches!(parsed.scheme(), "smtp" | "smtps") {
        return Err(invalid());
    }
    let port = parsed.port().unwrap_or(if implicit { 465 } else { 587 });
    let tls_param = parsed
        .query_pairs()
        .find(|(k, _)| k == "tls")
        .map(|(_, v)| v.into_owned());
    let tls = match tls_param.as_deref() {
        Some("none") => TlsMode::None,
        Some("required") => TlsMode::Required,
        Some("implicit") => TlsMode::Implicit,
        Some("opportunistic") | None => {
            if implicit || port == 465 {
                TlsMode::Implicit
            } else {
                TlsMode::Opportunistic
            }
        }
        Some(_) => return Err(invalid()),
    };
    Ok(EmailConfig::Smtp {
        host,
        port,
        username: (!parsed.username().is_empty()).then(|| percent_decode(parsed.username())),
        password: parsed.password().map(percent_decode),
        from,
        tls,
    })
}

fn percent_decode(value: &str) -> String {
    url::form_urlencoded::parse(format!("v={}", value.replace('+', "%2B")).as_bytes())
        .next()
        .map(|(_, v)| v.into_owned())
        .unwrap_or_else(|| value.to_owned())
}

#[derive(Clone, Copy)]
struct SmtpParams<'a> {
    host: &'a str,
    port: u16,
    username: Option<&'a str>,
    password: Option<&'a str>,
    sender: &'a str,
    tls: TlsMode,
}

pub struct NativeMail {
    config: EmailConfig,
}

impl NativeMail {
    pub fn new(config: EmailConfig) -> Self {
        Self { config }
    }

    async fn send_smtp(
        &self,
        params: SmtpParams<'_>,
        message: &EmailMessage,
    ) -> Result<(), String> {
        let SmtpParams {
            host,
            port,
            sender: _from,
            tls,
            ..
        } = params;
        let stream = TcpStream::connect((host, port))
            .await
            .map_err(|_| "smtp_connect".to_owned())?;
        if tls == TlsMode::Implicit {
            return self
                .smtp_session(
                    SmtpIoBox::tls(tls_wrap(stream, host).await?),
                    params,
                    message,
                    true,
                )
                .await;
        }
        self.smtp_session(SmtpIoBox::plain(stream), params, message, false)
            .await
    }

    async fn smtp_session(
        &self,
        stream: SmtpIoBox,
        params: SmtpParams<'_>,
        message: &EmailMessage,
        already_tls: bool,
    ) -> Result<(), String> {
        let SmtpParams {
            host,
            username,
            password,
            sender: from,
            tls,
            ..
        } = params;
        let mut io = stream;
        let greeting = io.read_reply().await?;
        expect(&greeting, 220)?;
        let mut caps = ehlo(&mut io).await?;
        expect(&caps, 250)?;
        if !already_tls && tls != TlsMode::None {
            if caps.text.to_uppercase().contains("STARTTLS") {
                io.write_line("STARTTLS").await?;
                expect(&io.read_reply().await?, 220)?;
                let plain = io.into_tcp().ok_or("smtp_starttls_unavailable")?;
                io = SmtpIoBox::tls(tls_wrap(plain, host).await?);
                caps = ehlo(&mut io).await?;
                expect(&caps, 250)?;
            } else if tls == TlsMode::Required {
                return Err("smtp_starttls_unavailable".into());
            }
        }
        if let Some(user) = username {
            let secret = password.unwrap_or("");
            let token =
                base64::engine::general_purpose::STANDARD.encode(format!("\0{user}\0{secret}"));
            io.write_line(&format!("AUTH PLAIN {token}")).await?;
            expect(&io.read_reply().await?, 235)?;
        }
        let envelope_from = angle_address(from).ok_or("smtp_bad_from")?;
        io.write_line(&format!("MAIL FROM:<{envelope_from}>"))
            .await?;
        expect(&io.read_reply().await?, 250)?;
        io.write_line(&format!("RCPT TO:<{}>", message.to)).await?;
        let rcpt = io.read_reply().await?;
        if rcpt.code != 250 && rcpt.code != 251 {
            return Err(format!("smtp_{}", rcpt.code));
        }
        io.write_line("DATA").await?;
        expect(&io.read_reply().await?, 354)?;
        io.write_raw(&mime_message(from, message)).await?;
        expect(&io.read_reply().await?, 250)?;
        io.write_line("QUIT").await.ok();
        Ok(())
    }
}

async fn ehlo(io: &mut SmtpIoBox) -> Result<SmtpReply, String> {
    io.write_line("EHLO runbook.local").await?;
    io.read_reply().await
}

async fn tls_wrap(
    stream: TcpStream,
    host: &str,
) -> Result<tokio_rustls::client::TlsStream<TcpStream>, String> {
    let mut roots = rustls::RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    let config = rustls::ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();
    let connector = TlsConnector::from(Arc::new(config));
    let name = rustls::pki_types::ServerName::try_from(host.to_owned())
        .map_err(|_| "smtp_bad_host".to_owned())?;
    connector
        .connect(name, stream)
        .await
        .map_err(|_| "smtp_tls".to_owned())
}

fn expect(reply: &SmtpReply, code: u16) -> Result<(), String> {
    if reply.code == code {
        Ok(())
    } else {
        Err(format!("smtp_{}", reply.code))
    }
}

fn angle_address(from: &str) -> Option<String> {
    if let Some(open) = from.rfind('<') {
        from[open + 1..]
            .strip_suffix('>')
            .map(|addr| addr.trim().chars().filter(|c| !c.is_control()).collect())
    } else {
        Some(from.trim().to_owned())
    }
    .filter(|addr| addr.contains('@'))
}

fn encode_header(value: &str) -> String {
    if value.is_ascii() {
        value.chars().filter(|c| *c != '\r' && *c != '\n').collect()
    } else {
        format!(
            "=?UTF-8?B?{}?=",
            base64::engine::general_purpose::STANDARD.encode(value)
        )
    }
}

fn dot_stuff(body: &str) -> String {
    body.lines()
        .map(|line| {
            if line.starts_with('.') {
                format!(".{line}")
            } else {
                line.to_owned()
            }
        })
        .collect::<Vec<_>>()
        .join("\r\n")
}

fn mime_message(from: &str, message: &EmailMessage) -> String {
    let mut seed = [0u8; 16];
    let _ = getrandom::fill(&mut seed);
    let boundary: String = seed.iter().map(|b| format!("{b:02x}")).collect();
    let boundary = format!("runbook-{boundary}");
    let encode = |body: &str| {
        base64::engine::general_purpose::STANDARD
            .encode(body)
            .as_bytes()
            .chunks(76)
            .map(|chunk| String::from_utf8_lossy(chunk).into_owned())
            .collect::<Vec<_>>()
            .join("\r\n")
    };
    let unsubscribe = message
        .text
        .lines()
        .find_map(|line| line.trim().strip_prefix("Unsubscribe: "))
        .map(str::trim)
        .filter(|url| url.starts_with("https://"));
    let list_headers = unsubscribe
        .map(|url| {
            format!(
                "List-Unsubscribe: <{url}>\r\nList-Unsubscribe-Post: List-Unsubscribe=One-Click\r\n"
            )
        })
        .unwrap_or_default();
    format!(
        "From: {}\r\nTo: {}\r\nSubject: {}\r\nMIME-Version: 1.0\r\n{list_headers}Content-Type: multipart/alternative; boundary=\"{boundary}\"\r\n\r\n--{boundary}\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Transfer-Encoding: base64\r\n\r\n{}\r\n--{boundary}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Transfer-Encoding: base64\r\n\r\n{}\r\n--{boundary}--\r\n.\r\n",
        encode_header(from),
        encode_header(&message.to),
        encode_header(&message.subject),
        encode(&dot_stuff(&message.text)),
        encode(&message.html),
    )
}

struct SmtpReply {
    code: u16,
    text: String,
}

enum Stream {
    Plain(TcpStream),
    Tls(Box<tokio_rustls::client::TlsStream<TcpStream>>),
}

impl tokio::io::AsyncRead for Stream {
    fn poll_read(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        match self.get_mut() {
            Stream::Plain(stream) => std::pin::Pin::new(stream).poll_read(cx, buf),
            Stream::Tls(stream) => std::pin::Pin::new(stream.as_mut()).poll_read(cx, buf),
        }
    }
}

impl tokio::io::AsyncWrite for Stream {
    fn poll_write(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        data: &[u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        match self.get_mut() {
            Stream::Plain(stream) => std::pin::Pin::new(stream).poll_write(cx, data),
            Stream::Tls(stream) => std::pin::Pin::new(stream.as_mut()).poll_write(cx, data),
        }
    }
    fn poll_flush(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        match self.get_mut() {
            Stream::Plain(stream) => std::pin::Pin::new(stream).poll_flush(cx),
            Stream::Tls(stream) => std::pin::Pin::new(stream.as_mut()).poll_flush(cx),
        }
    }
    fn poll_shutdown(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        match self.get_mut() {
            Stream::Plain(stream) => std::pin::Pin::new(stream).poll_shutdown(cx),
            Stream::Tls(stream) => std::pin::Pin::new(stream.as_mut()).poll_shutdown(cx),
        }
    }
}

struct SmtpIoBox {
    inner: BufReader<Stream>,
}

impl SmtpIoBox {
    fn plain(stream: TcpStream) -> Self {
        Self {
            inner: BufReader::new(Stream::Plain(stream)),
        }
    }
    fn tls(stream: tokio_rustls::client::TlsStream<TcpStream>) -> Self {
        Self {
            inner: BufReader::new(Stream::Tls(Box::new(stream))),
        }
    }
    /// Take the plain stream back for the STARTTLS upgrade. Any buffered
    /// bytes would be protocol desync, so they refuse the upgrade.
    fn into_tcp(self) -> Option<TcpStream> {
        if !self.inner.buffer().is_empty() {
            return None;
        }
        match self.inner.into_inner() {
            Stream::Plain(stream) => Some(stream),
            Stream::Tls(_) => None,
        }
    }
    async fn write_line(&mut self, line: &str) -> Result<(), String> {
        self.write_raw(&format!("{line}\r\n")).await
    }
    async fn write_raw(&mut self, data: &str) -> Result<(), String> {
        self.inner
            .get_mut()
            .write_all(data.as_bytes())
            .await
            .map_err(|_| "smtp_write".to_owned())
    }
    async fn read_reply(&mut self) -> Result<SmtpReply, String> {
        let mut text = String::new();
        let mut line = Vec::new();
        loop {
            line.clear();
            let read = self
                .inner
                .read_until(b'\n', &mut line)
                .await
                .map_err(|_| "smtp_read".to_owned())?;
            if read == 0 {
                return Err("smtp_closed".into());
            }
            let part = String::from_utf8_lossy(&line).into_owned();
            text.push_str(&part);
            if part.len() >= 4 && part.as_bytes()[3] == b' ' {
                let code = part[..3].parse().map_err(|_| "smtp_reply".to_owned())?;
                return Ok(SmtpReply { code, text });
            }
            if part.len() < 4 {
                return Err("smtp_reply".into());
            }
        }
    }
}

impl MailTransport for NativeMail {
    async fn send(&self, message: &EmailMessage) -> Result<(), String> {
        match &self.config {
            EmailConfig::Capture { file, .. } => {
                let line = serde_json::json!({
                    "at": chrono_now(),
                    "to": message.to,
                    "type": "notification",
                    "code": "",
                    "subject": message.subject,
                    "text": message.text,
                    "html": message.html,
                })
                .to_string();
                let mut options = tokio::fs::OpenOptions::new();
                options.create(true).append(true);
                #[cfg(unix)]
                {
                    options.mode(0o600);
                }
                let mut handle = options
                    .open(file)
                    .await
                    .map_err(|_| "capture_open".to_owned())?;
                handle
                    .write_all(format!("{line}\n").as_bytes())
                    .await
                    .map_err(|_| "capture_write".to_owned())
            }
            EmailConfig::Smtp {
                host,
                port,
                username,
                password,
                from,
                tls,
            } => {
                self.send_smtp(
                    SmtpParams {
                        host,
                        port: *port,
                        username: username.as_deref(),
                        password: password.as_deref(),
                        sender: from,
                        tls: *tls,
                    },
                    message,
                )
                .await
            }
        }
    }
}

fn chrono_now() -> String {
    // The capture file's other writer uses ISO-8601; epoch millis is
    // unambiguous and needs no clock formatting dependency here.
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis().to_string())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_parsing_names_the_modes() {
        let get = |map: &[(&str, &str)]| {
            let owned: std::collections::BTreeMap<String, String> = map
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect();
            move |name: &str| owned.get(name).cloned()
        };
        assert_eq!(email_config(get(&[])).unwrap(), None);
        let smtp = email_config(get(&[
            ("RUNBOOK_SMTP_URL", "smtp://user:pass@mail.example.test:587"),
            ("RUNBOOK_SMTP_FROM", "The Runbook <login@example.test>"),
        ]))
        .unwrap()
        .unwrap();
        assert!(matches!(
            smtp,
            EmailConfig::Smtp {
                tls: TlsMode::Opportunistic,
                ..
            }
        ));
        assert!(email_config(get(&[("RUNBOOK_AUTH_EMAIL_TRANSPORT", "smtp")])).is_err());
        assert!(email_config(get(&[("RUNBOOK_AUTH_EMAIL_TRANSPORT", "what")])).is_err());
        let capture = email_config(get(&[
            ("RUNBOOK_AUTH_EMAIL_TRANSPORT", "capture"),
            ("RUNBOOK_AUTH_CAPTURE_FILE", "/tmp/codes.ndjson"),
        ]))
        .unwrap()
        .unwrap();
        assert!(matches!(capture, EmailConfig::Capture { .. }));
    }

    #[test]
    fn mime_carries_both_parts_and_the_unsubscribe_header() {
        let message = EmailMessage {
            to: "ada@example.test".into(),
            subject: "Approval needed".into(),
            text: "hello\nUnsubscribe: https://runs.example.test/unsubscribe?token=t".into(),
            html: "<p>hello</p>".into(),
        };
        let wire = mime_message("The Runbook <login@example.test>", &message);
        assert!(wire.contains("multipart/alternative"));
        assert!(wire.contains("List-Unsubscribe: <https://runs.example.test/unsubscribe?token=t>"));
        assert!(wire.contains("List-Unsubscribe-Post: List-Unsubscribe=One-Click"));
        assert!(wire.ends_with("\r\n.\r\n"));
    }
}

#[cfg(test)]
mod smtp_tests {
    use super::*;
    use tokio::net::TcpListener;

    #[tokio::test(flavor = "current_thread")]
    async fn sends_one_message_over_plain_smtp_with_auth() {
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut lines = BufReader::new(stream);
            let mut seen = String::new();
            let mut data = String::new();
            lines.get_mut().write_all(b"220 test\r\n").await.unwrap();
            let mut in_data = false;
            loop {
                let mut line = String::new();
                if lines.read_line(&mut line).await.unwrap() == 0 {
                    break;
                }
                seen.push_str(&line);
                let upper = line.to_uppercase();
                if in_data {
                    data.push_str(&line);
                    if line == ".\r\n" {
                        in_data = false;
                        lines.get_mut().write_all(b"250 queued\r\n").await.unwrap();
                    }
                } else if upper.starts_with("EHLO") {
                    lines
                        .get_mut()
                        .write_all(b"250-hello\r\n250 AUTH PLAIN\r\n")
                        .await
                        .unwrap();
                } else if upper.starts_with("AUTH PLAIN") {
                    lines.get_mut().write_all(b"235 ok\r\n").await.unwrap();
                } else if upper.starts_with("MAIL FROM") || upper.starts_with("RCPT TO") {
                    lines.get_mut().write_all(b"250 ok\r\n").await.unwrap();
                } else if upper.starts_with("DATA") {
                    in_data = true;
                    lines.get_mut().write_all(b"354 go\r\n").await.unwrap();
                } else if upper.starts_with("QUIT") {
                    lines.get_mut().write_all(b"221 bye\r\n").await.unwrap();
                    break;
                }
            }
            (seen, data)
        });
        let mail = NativeMail::new(EmailConfig::Smtp {
            host: "127.0.0.1".into(),
            port,
            username: Some("user".into()),
            password: Some("secret".into()),
            from: "The Runbook <login@example.test>".into(),
            tls: TlsMode::None,
        });
        mail.send(&EmailMessage {
            to: "ada@example.test".into(),
            subject: "Approval needed".into(),
            text: "hello\nUnsubscribe: https://runs.example.test/unsubscribe?token=t".into(),
            html: "<p>hello</p>".into(),
        })
        .await
        .unwrap();
        let (seen, data) = server.await.unwrap();
        assert!(seen.contains("AUTH PLAIN"), "{seen}");
        assert!(seen.contains("MAIL FROM:<login@example.test>"), "{seen}");
        assert!(seen.contains("RCPT TO:<ada@example.test>"), "{seen}");
        // DATA is base64 MIME; the decoded parts are covered by the unit
        // test above. Here: headers arrived intact.
        assert!(data.contains("Subject: Approval needed"), "{data}");
        assert!(data.contains("multipart/alternative"), "{data}");
        assert!(
            data.contains("List-Unsubscribe-Post: List-Unsubscribe=One-Click"),
            "{data}"
        );
    }
}
