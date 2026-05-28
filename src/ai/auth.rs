use std::sync::Arc;

use parking_lot::RwLock;
use secrecy::SecretString;

#[derive(Clone)]
pub enum AuthStrategy {
    ApiKey(SecretString),
    OAuthDevice(Arc<OAuthState>),
    None,
}

pub struct OAuthState {
    pub github_token: RwLock<Option<SecretString>>,
    pub session_token: RwLock<Option<CachedToken>>,
}

#[derive(Clone, Debug)]
pub struct CachedToken {
    pub value: SecretString,
    pub expires_at_unix: i64,
}

impl OAuthState {
    pub fn new() -> Self {
        Self {
            github_token: RwLock::new(None),
            session_token: RwLock::new(None),
        }
    }
}

impl Default for OAuthState {
    fn default() -> Self {
        Self::new()
    }
}

// ========================================================================
// Tests
// ========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_oauth_state_default() {
        let state = OAuthState::default();
        assert!(state.github_token.read().is_none());
        assert!(state.session_token.read().is_none());
    }
}
