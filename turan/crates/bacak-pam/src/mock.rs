//! A dependency-free authenticator for tests, CI and `--no-default-features`
//! development builds. It validates against an in-memory credential table and
//! exercises the exact same [`Conversation`] flow the real backend uses.

use crate::{AuthError, AuthResult, AuthedUser, Authenticator, Conversation, Prompt};
use std::collections::HashMap;

/// Canned, in-memory authenticator. **Never** compiled into the privileged
/// daemon's release build path; it exists for tests and local UI iteration.
#[derive(Default)]
pub struct MockAuthenticator {
    /// username -> password
    credentials: HashMap<String, String>,
    authed: Option<String>,
    session_open: bool,
}

impl MockAuthenticator {
    pub fn with_user(mut self, user: &str, password: &str) -> Self {
        self.credentials.insert(user.into(), password.into());
        self
    }
}

impl Authenticator for MockAuthenticator {
    fn authenticate(
        &mut self,
        username: &str,
        conv: &mut dyn Conversation,
    ) -> AuthResult<AuthedUser> {
        let expected = self
            .credentials
            .get(username)
            .ok_or(AuthError::AuthFailed)?
            .clone();

        let answer = conv
            .handle(&Prompt::SecretInput("Password: ".into()))?
            .ok_or(AuthError::NoAnswer)?;

        if answer.expose() == expected {
            self.authed = Some(username.to_string());
            Ok(AuthedUser {
                username: username.to_string(),
            })
        } else {
            conv.handle(&Prompt::Error("Authentication failed".into()))?;
            Err(AuthError::AuthFailed)
        }
    }

    fn open_session(&mut self) -> AuthResult<Vec<(String, String)>> {
        if self.authed.is_none() {
            return Err(AuthError::Aborted);
        }
        self.session_open = true;
        Ok(vec![("BDM_MOCK_SESSION".into(), "1".into())])
    }

    fn close_session(&mut self) -> AuthResult<()> {
        self.session_open = false;
        self.authed = None;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bacak_common::ipc::Secret;

    /// A conversation that replies with a fixed secret.
    struct Fixed(&'static str);
    impl Conversation for Fixed {
        fn handle(&mut self, prompt: &Prompt) -> AuthResult<Option<Secret>> {
            match prompt {
                Prompt::SecretInput(_) | Prompt::VisibleInput(_) => Ok(Some(self.0.into())),
                Prompt::Info(_) | Prompt::Error(_) => Ok(None),
            }
        }
    }

    #[test]
    fn correct_password_authenticates_and_opens_session() {
        let mut auth = MockAuthenticator::default().with_user("ayse", "hunter2");
        let user = auth.authenticate("ayse", &mut Fixed("hunter2")).unwrap();
        assert_eq!(user.username, "ayse");
        let env = auth.open_session().unwrap();
        assert!(env.iter().any(|(k, _)| k == "BDM_MOCK_SESSION"));
        auth.close_session().unwrap();
    }

    #[test]
    fn wrong_password_is_rejected() {
        let mut auth = MockAuthenticator::default().with_user("ayse", "hunter2");
        let err = auth.authenticate("ayse", &mut Fixed("nope")).unwrap_err();
        assert!(matches!(err, AuthError::AuthFailed));
    }

    #[test]
    fn unknown_user_is_rejected_without_leaking_which() {
        let mut auth = MockAuthenticator::default().with_user("ayse", "hunter2");
        let err = auth
            .authenticate("ghost", &mut Fixed("hunter2"))
            .unwrap_err();
        // same error variant as a wrong password -> no user-enumeration oracle
        assert!(matches!(err, AuthError::AuthFailed));
    }

    #[test]
    fn cannot_open_session_without_auth() {
        let mut auth = MockAuthenticator::default();
        assert!(auth.open_session().is_err());
    }
}
