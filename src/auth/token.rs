//! The [`Token`] newtype (T-101, EPIC-02).
//!
//! Wraps the Todoist API token in the existing [`crate::log::Secret`]
//! redaction helper (T-006) so it can never be accidentally logged or
//! printed: `Display`/`Debug` render exactly [`crate::log::REDACTED`]
//! (`***`), reusing `Secret`'s formatting rather than reimplementing it.
//! The single audited read path is [`Token::expose`].

use std::fmt;

use crate::error::{AuthError, Result};
use crate::log::Secret;

/// A validated, non-empty Todoist API token.
///
/// Constructible only from an owned [`String`] via [`Token::new`]; the
/// inner field is private, so [`Token::expose`] is the only way to read the
/// raw value back out.
pub struct Token(Secret<String>);

impl Token {
    /// Builds a `Token` from an owned string.
    ///
    /// # Errors
    ///
    /// Returns [`AuthError::EmptyToken`] when `value` is empty or
    /// whitespace-only.
    pub fn new(value: String) -> Result<Self> {
        if value.trim().is_empty() {
            return Err(AuthError::EmptyToken.into());
        }
        Ok(Self(Secret::new(value)))
    }

    /// Explicit, greppable access to the raw token.
    #[must_use]
    pub fn expose(&self) -> &str {
        self.0.expose()
    }
}

impl fmt::Display for Token {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.0, f)
    }
}

impl fmt::Debug for Token {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&self.0, f)
    }
}

#[cfg(test)]
mod tests {
    //! Sanity checks proving the T-101 design constraints hold; the
    //! unit-tester adds the full suite separately.
    use super::*;
    use crate::error::AppError;

    const FIXTURE_TOKEN: &str = "tok_fake_abc123";

    #[test]
    fn new_accepts_non_empty_and_exposes_it_back() {
        let token = Token::new(FIXTURE_TOKEN.to_string()).expect("non-empty token");
        assert_eq!(token.expose(), FIXTURE_TOKEN);
    }

    #[test]
    fn new_rejects_empty_and_whitespace_only() {
        assert!(matches!(
            Token::new(String::new()),
            Err(AppError::Auth(crate::error::AuthError::EmptyToken))
        ));
        assert!(matches!(
            Token::new("   \t\n".to_string()),
            Err(AppError::Auth(crate::error::AuthError::EmptyToken))
        ));
    }

    #[test]
    fn display_and_debug_render_exactly_redacted_marker() {
        let token = Token::new(FIXTURE_TOKEN.to_string()).expect("non-empty token");
        assert_eq!(format!("{token}"), crate::log::REDACTED);
        assert_eq!(format!("{token:?}"), crate::log::REDACTED);
        assert!(!format!("{token}").contains(FIXTURE_TOKEN));
        assert!(!format!("{token:?}").contains(FIXTURE_TOKEN));
    }
}
