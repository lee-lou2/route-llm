use super::*;

pub(super) fn prefixed_id(prefix: &str) -> String {
    match db::generate_client_api_key() {
        Ok(value) => format!("{prefix}_{}", &value[..32]),
        Err(_) => format!("{prefix}_{}", db::now_epoch()),
    }
}
