use anyhow::{Context, bail};
use sha2::{Digest, Sha256};
use std::time::SystemTime;

#[cfg(test)]
use super::ModelAliasSummary;
#[cfg(test)]
use sqlx::Row;

pub(crate) fn api_key_fingerprint(api_key_hash: &str) -> String {
    let short_hash: String = api_key_hash.chars().take(12).collect();
    format!("sha256:{short_hash}")
}

#[cfg(test)]
pub(crate) fn model_alias_summary_from_row(row: sqlx::sqlite::SqliteRow) -> ModelAliasSummary {
    ModelAliasSummary {
        id: row.get("id"),
        public_model: row.get("public_model"),
        target_type: row.get("target_type"),
        enabled: row.get::<i64, _>("enabled") == 1,
        created_at: row.get("created_at"),
        updated_at: row.get("updated_at"),
        routes: Vec::new(),
    }
}

pub fn now_epoch() -> i64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .expect("system clock before unix epoch")
        .as_secs() as i64
}

pub fn mask_secret(secret: &str) -> String {
    let chars: Vec<char> = secret.chars().collect();
    if chars.len() <= 8 {
        return "****".to_string();
    }
    let start: String = chars.iter().take(4).collect();
    let end: String = chars.iter().skip(chars.len() - 4).collect();
    format!("{start}...{end}")
}

pub fn hash_secret(secret: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(secret.as_bytes());
    hex::encode(hasher.finalize())
}

pub(crate) fn validate_base_url(base_url: &str) -> anyhow::Result<()> {
    let parsed = url::Url::parse(base_url).context("base_url must be an absolute URL")?;
    match parsed.scheme() {
        "http" | "https" => Ok(()),
        scheme => bail!("base_url scheme must be http or https, got {scheme}"),
    }
}

pub(crate) fn truncate_error(error: &str) -> String {
    const MAX_LEN: usize = 500;
    if error.len() <= MAX_LEN {
        return error.to_string();
    }
    error.chars().take(MAX_LEN).collect()
}
