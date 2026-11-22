//! Is the model there, which one is it, and how much context does it have?
//!
//! `ai.py::server_ok` / `model_id` answered two of those questions with one GET.
//! The context size is the third, and it matters because every trimming decision
//! in [`crate::context`] is a guess about a window this crate cannot see: a model
//! server loaded with `-c 4096` does not fit the same prompt as one loaded with
//! `-c 16384`.
//!
//! llama.cpp answers `/v1/models` for the id and `/props` for
//! `default_generation_settings.n_ctx` and `total_slots`. The second is optional:
//! a server that does not serve it still counts as answering.

use crate::chat::LlamaClient;
use crate::error::AiError;

/// What `ai.py` fell back to when the server would not name itself.
pub const UNKNOWN_MODEL: &str = "local";

/// llama.cpp's default `-c` when nothing is passed. Used only for a status line,
/// never to decide that a prompt fits.
pub const ASSUMED_CONTEXT: u64 = 6144;

/// The three answers a health check owes a caller.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerInfo {
    /// `/v1/models` answered.
    pub answering: bool,
    /// The id the server reported, or the id this client asked for.
    pub model: String,
    /// `n_ctx` from `/props`, when the server reports it.
    pub context_size: Option<u64>,
    /// `total_slots` from `/props`, when the server reports it.
    pub slots: Option<u64>,
}

impl ServerInfo {
    /// A server that did not answer. Never an error: "not running" is a fact the
    /// UI shows, not a failure to report.
    pub fn offline(model: &str) -> ServerInfo {
        ServerInfo {
            answering: false,
            model: if model.is_empty() {
                UNKNOWN_MODEL.to_string()
            } else {
                model.to_string()
            },
            context_size: None,
            slots: None,
        }
    }

    /// The window to assume when the server does not say. Only for a status line.
    pub fn context_or_assumed(&self) -> u64 {
        self.context_size.unwrap_or(ASSUMED_CONTEXT)
    }

    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "answering": self.answering,
            "model": self.model,
            "context_size": self.context_size,
            "slots": self.slots,
        })
    }
}

/// `ai.py::server_ok` — is the model answering?
pub fn server_ok(client: &dyn LlamaClient) -> bool {
    client.probe().map(|info| info.answering).unwrap_or(false)
}

/// `ai.py::model_id` — the server's id, or the configured one, or `"local"`.
///
/// Never an empty string: every caller writes this into a notification or a log
/// line, and a blank model name there is worse than the word "local".
pub fn model_id(client: &dyn LlamaClient) -> String {
    match client.probe() {
        Ok(info) if !info.model.trim().is_empty() => info.model,
        Ok(_) => fallback(client),
        Err(_) => fallback(client),
    }
}

fn fallback(client: &dyn LlamaClient) -> String {
    let model = client.model().trim();
    if model.is_empty() {
        UNKNOWN_MODEL.to_string()
    } else {
        model.to_string()
    }
}

/// The full check, with the error kept for a caller that wants to say why.
pub fn check(client: &dyn LlamaClient) -> (ServerInfo, Option<AiError>) {
    match client.probe() {
        Ok(info) => (info, None),
        Err(error) => (ServerInfo::offline(client.model()), Some(error)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chat::{ChatReply, ChatRequest};
    use crate::Control;

    struct Offline;

    impl LlamaClient for Offline {
        fn model(&self) -> &str {
            "qwen-local"
        }
        fn complete(&self, _request: &ChatRequest) -> Result<ChatReply, AiError> {
            Err(AiError::Unreachable("nothing listening".into()))
        }
        fn complete_stream(
            &self,
            _request: &ChatRequest,
            _on_delta: &mut dyn FnMut(&str) -> Control,
        ) -> Result<ChatReply, AiError> {
            Err(AiError::Unreachable("nothing listening".into()))
        }
        fn probe(&self) -> Result<ServerInfo, AiError> {
            Err(AiError::Unreachable("nothing listening".into()))
        }
    }

    struct Online;

    impl LlamaClient for Online {
        fn model(&self) -> &str {
            "configured"
        }
        fn complete(&self, _request: &ChatRequest) -> Result<ChatReply, AiError> {
            Ok(ChatReply::default())
        }
        fn complete_stream(
            &self,
            _request: &ChatRequest,
            _on_delta: &mut dyn FnMut(&str) -> Control,
        ) -> Result<ChatReply, AiError> {
            Ok(ChatReply::default())
        }
        fn probe(&self) -> Result<ServerInfo, AiError> {
            Ok(ServerInfo {
                answering: true,
                model: "Qwen3.5-4B-Q4_K_M".to_string(),
                context_size: Some(6144),
                slots: Some(4),
            })
        }
    }

    #[test]
    fn a_running_model_is_reported_with_its_window() {
        assert!(server_ok(&Online));
        assert_eq!(model_id(&Online), "Qwen3.5-4B-Q4_K_M");
        let (info, error) = check(&Online);
        assert!(error.is_none());
        assert_eq!(info.context_size, Some(6144));
        assert_eq!(info.context_or_assumed(), 6144);
    }

    #[test]
    fn a_model_that_is_not_there_is_a_fact_not_an_error() {
        assert!(!server_ok(&Offline));
        assert_eq!(model_id(&Offline), "qwen-local");
        let (info, error) = check(&Offline);
        assert!(!info.answering);
        assert!(error.is_some());
        assert_eq!(info.context_or_assumed(), ASSUMED_CONTEXT);
        assert_eq!(info.to_json()["answering"], serde_json::json!(false));
    }
}
