use super::*;
use axum::body::{Body, Bytes};
use futures_util::StreamExt;
use sqlx::SqlitePool;

#[derive(Clone)]
pub(crate) struct StreamUsageAuditHandle {
    audit_id: Arc<Mutex<Option<i64>>>,
}

impl StreamUsageAuditHandle {
    pub(crate) fn new() -> Self {
        Self {
            audit_id: Arc::new(Mutex::new(None)),
        }
    }

    pub(crate) fn set_audit_id(&self, audit_id: i64) {
        if let Ok(mut current) = self.audit_id.lock() {
            *current = Some(audit_id);
        }
    }

    pub(crate) fn audit_id(&self) -> Option<i64> {
        self.audit_id.lock().ok().and_then(|current| *current)
    }
}

pub(super) fn capture_stream_usage_body(
    pool: SqlitePool,
    stream: impl futures_util::Stream<Item = Result<Bytes, reqwest::Error>> + Send + 'static,
    audit_handle: StreamUsageAuditHandle,
) -> Body {
    let mut stream = Box::pin(stream);
    let body_stream = async_stream::stream! {
        let mut parser = SseUsageParser::default();
        while let Some(chunk) = stream.next().await {
            match chunk {
                Ok(chunk) => {
                    parser.push_bytes(&chunk);
                    yield Ok::<Bytes, std::io::Error>(chunk);
                }
                Err(error) => {
                    yield Err(std::io::Error::other(error.to_string()));
                    return;
                }
            }
        }
        if let (Some(audit_id), Some(usage)) = (audit_handle.audit_id(), parser.finish())
            && let Err(error) = db::update_request_audit_usage(
                &pool,
                audit_id,
                usage.input_tokens,
                usage.output_tokens,
                usage.total_tokens,
            )
            .await
            {
                tracing::warn!(error = %error, audit_id, "failed to update streaming token usage");
        }
    };
    Body::from_stream(body_stream)
}

#[derive(Default)]
pub(super) struct SseUsageParser {
    buffer: String,
    usage: Option<TokenUsage>,
}

impl SseUsageParser {
    pub(super) fn push_bytes(&mut self, chunk: &Bytes) {
        self.buffer.push_str(&String::from_utf8_lossy(chunk));
        while let Some(index) = self.buffer.find('\n') {
            let mut line = self.buffer.drain(..=index).collect::<String>();
            if line.ends_with('\n') {
                line.pop();
            }
            if line.ends_with('\r') {
                line.pop();
            }
            self.observe_line(&line);
        }
    }

    pub(super) fn finish(mut self) -> Option<TokenUsage> {
        if !self.buffer.is_empty() {
            let line = std::mem::take(&mut self.buffer);
            self.observe_line(&line);
        }
        self.usage
    }

    fn observe_line(&mut self, line: &str) {
        let Some(data) = line.strip_prefix("data:") else {
            return;
        };
        let data = data.trim();
        if data.is_empty() || data == "[DONE]" {
            return;
        }
        let Ok(value) = serde_json::from_str::<Value>(data) else {
            return;
        };
        if let Some(usage) = extract_token_usage_from_value(&value) {
            self.usage = Some(usage);
        }
    }
}
