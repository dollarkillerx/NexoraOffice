//! Blocking HTTP for the providers: one ureq agent with rustls (ring) and
//! the platform's certificate verifier, and a response body read on a
//! helper thread so the caller can stop waiting at once when the user
//! cancels, even while the server is silent.
//!
//! The helper thread owns the connection. When the caller gives up, the
//! receiving end is dropped; the thread notices at its next read (or when
//! the read idle timeout ends it) and closes the connection.

use std::io::Read;
use std::sync::Once;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::thread;
use std::time::Duration;

use ureq::Agent;
use ureq::tls::{RootCerts, TlsConfig, TlsProvider};

use crate::error::AiError;

/// How long to wait for a connection.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
/// How long a read may wait for the next bytes. Streams have no total
/// timeout, since a long answer can take many minutes, but a server that
/// says nothing for this long is taken as gone. Models that think before
/// answering send keep-alive pings well within it.
const IDLE_TIMEOUT: Duration = Duration::from_secs(120);
/// How often a waiting caller looks at the cancel flag.
const POLL: Duration = Duration::from_millis(20);
/// The most of an error body read; it only feeds a short message.
const MAX_ERROR_BODY: u64 = 64 * 1024;
/// The most of a whole (non-streamed) body read, such as a model list.
const MAX_BODY: usize = 8 * 1024 * 1024;

static CRYPTO: Once = Once::new();

/// Make ring rustls's process-wide crypto provider. Another part of the
/// program may have installed one already, which is just as good.
fn install_crypto_provider() {
    CRYPTO.call_once(|| {
        let _ = rustls::crypto::ring::default_provider().install_default();
    });
}

/// The HTTP agent the providers share their settings with. `http://` works
/// too, for local models.
pub(crate) fn agent() -> Agent {
    install_crypto_provider();
    let tls = TlsConfig::builder()
        .provider(TlsProvider::Rustls)
        .root_certs(RootCerts::PlatformVerifier)
        .build();
    Agent::config_builder()
        .tls_config(tls)
        .timeout_connect(Some(CONNECT_TIMEOUT))
        .timeout_per_read(Some(IDLE_TIMEOUT))
        // Error statuses are read like any response, for the provider's
        // message in the body.
        .http_status_as_error(false)
        // A redirect would carry the request, API key headers included, to
        // whatever host it names (ureq drops only `Authorization` across
        // hosts, not `x-api-key`). Provider APIs do not redirect, so a 3xx
        // is reported as the error it is.
        .max_redirects(0)
        .user_agent(concat!("AsterOffice/", env!("CARGO_PKG_VERSION")))
        .build()
        .into()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Method {
    Get,
    Post,
}

/// One request. Headers may hold the API key, so this has no `Debug`.
pub(crate) struct HttpRequest {
    pub method: Method,
    pub url: String,
    pub headers: Vec<(&'static str, String)>,
    pub body: Option<String>,
}

enum Chunk {
    Bytes(Vec<u8>),
    End,
    Failed(AiError),
}

/// A response body arriving in chunks of bytes, as the server sends them.
pub(crate) struct Body {
    rx: Receiver<Chunk>,
    done: bool,
}

/// Send `request` on a helper thread. Errors, the HTTP status ones
/// included, come out of [`Body::next`].
pub(crate) fn send(agent: &Agent, request: HttpRequest) -> Body {
    let (tx, rx) = mpsc::channel();
    let agent = agent.clone();
    let spawned = thread::Builder::new()
        .name("aster-ai-http".into())
        .spawn(move || transfer(&agent, request, &tx));
    if let Err(error) = spawned {
        let (tx, rx) = mpsc::channel();
        let _ = tx.send(Chunk::Failed(AiError::Network(error.to_string())));
        return Body { rx, done: false };
    }
    Body { rx, done: false }
}

fn transfer(agent: &Agent, request: HttpRequest, tx: &Sender<Chunk>) {
    let result = match request.method {
        Method::Get => {
            let mut builder = agent.get(request.url.as_str());
            for (name, value) in &request.headers {
                builder = builder.header(*name, value.as_str());
            }
            builder.call()
        }
        Method::Post => {
            let mut builder = agent.post(request.url.as_str());
            for (name, value) in &request.headers {
                builder = builder.header(*name, value.as_str());
            }
            builder.send(request.body.unwrap_or_default())
        }
    };
    let response = match result {
        Ok(response) => response,
        Err(error) => {
            let _ = tx.send(Chunk::Failed(network_error(&error)));
            return;
        }
    };
    let status = response.status().as_u16();
    if !(200..300).contains(&status) {
        let retry_after = response
            .headers()
            .get("retry-after")
            .and_then(|value| value.to_str().ok())
            .map(str::to_string);
        let body = response
            .into_body()
            .into_with_config()
            .limit(MAX_ERROR_BODY)
            .read_to_string()
            .unwrap_or_default();
        let error = AiError::from_status(status, &body, retry_after.as_deref());
        let _ = tx.send(Chunk::Failed(error));
        return;
    }
    let mut reader = response.into_body().into_reader();
    let mut buffer = vec![0; 16 * 1024];
    loop {
        match reader.read(&mut buffer) {
            Ok(0) => {
                let _ = tx.send(Chunk::End);
                return;
            }
            Ok(read) => {
                if tx.send(Chunk::Bytes(buffer[..read].to_vec())).is_err() {
                    // The caller stopped listening: drop the connection.
                    return;
                }
            }
            Err(error) => {
                let _ = tx.send(Chunk::Failed(AiError::Network(error.to_string())));
                return;
            }
        }
    }
}

fn network_error(error: &ureq::Error) -> AiError {
    match error {
        ureq::Error::BadUri(_) | ureq::Error::Http(_) => {
            AiError::NotConfigured(format!("the address is not valid: {error}"))
        }
        _ => AiError::Network(error.to_string()),
    }
}

impl Body {
    /// The next chunk, `None` at the end. Returns [`AiError::Cancelled`] as
    /// soon as `cancel` is set, whatever the server is doing.
    pub(crate) fn next(&mut self, cancel: &AtomicBool) -> Result<Option<Vec<u8>>, AiError> {
        if self.done {
            return Ok(None);
        }
        loop {
            if cancel.load(Ordering::Relaxed) {
                self.done = true;
                return Err(AiError::Cancelled);
            }
            match self.rx.recv_timeout(POLL) {
                Ok(Chunk::Bytes(bytes)) => return Ok(Some(bytes)),
                Ok(Chunk::End) => {
                    self.done = true;
                    return Ok(None);
                }
                Ok(Chunk::Failed(error)) => {
                    self.done = true;
                    return Err(error);
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => {
                    self.done = true;
                    return Err(AiError::Network("the connection was lost".into()));
                }
            }
        }
    }

    /// The whole body as text, for small replies such as a model list.
    pub(crate) fn text(mut self, cancel: &AtomicBool) -> Result<String, AiError> {
        let mut bytes = Vec::new();
        while let Some(chunk) = self.next(cancel)? {
            bytes.extend_from_slice(&chunk);
            if bytes.len() > MAX_BODY {
                return Err(AiError::BadResponse("the reply is too large".into()));
            }
        }
        String::from_utf8(bytes).map_err(|_| AiError::BadResponse("the reply is not UTF-8".into()))
    }
}
