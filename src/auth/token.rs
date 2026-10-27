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

/// Extended T-101 coverage (unit-tester, different model family from the
/// coder). Complements — does not duplicate — the sanity `tests` module
/// above: whitespace-shape matrix, observed trim-vs-preserve behaviour,
/// exact-echo `expose`, redaction edge cases (very short tokens, tokens
/// containing `***` itself, realistic secret shapes), nested-Debug safety,
/// `Secret` delegation, and `Send + Sync` for later async wiring.
#[cfg(test)]
mod unit_tests {
    use super::*;
    use crate::error::AppError;

    /// Fake, never-real secret-shaped fixtures (hex digest, GitHub-PAT
    /// shape, bearer header, base64 blob, marker-colliding strings).
    const FAKE_SECRETS: [&str; 6] = [
        "deadbeefcafebabe1234567890abcdef",
        "ghp_fake16C7e42F292c6912E7710c838347Ae17",
        "Bearer fake 9f8e7d6c5b4a3928",
        "c2VjcmV0LWZha2UtdG9rZW4=",
        "tok***fake",
        "токе́н_fäke",
    ];

    #[test]
    fn new_rejects_every_whitespace_only_shape() {
        // ASCII shapes plus Unicode White_Space characters (NBSP, NEL,
        // ideographic space, VT/FF): `str::trim` removes all of them, so
        // every input below must be rejected as EmptyToken, never panic,
        // and never echo the input in the error.
        for ws in [
            "",
            " ",
            "  ",
            "\t",
            "\n",
            "\r\n",
            " \t \r\n ",
            "\u{b}",
            "\u{c}",
            "\u{85}",
            "\u{a0}",
            "\u{3000}",
        ] {
            match Token::new(ws.to_string()) {
                Err(AppError::Auth(AuthError::EmptyToken)) => {}
                other => panic!("input {ws:?} should be EmptyToken, got {other:?}"),
            }
        }
    }

    #[test]
    fn new_rejects_whitespace_mixed_with_marker_and_secret_shapes() {
        // Padding made of the redaction marker / secret-looking fragments
        // is still whitespace-only once trimmed and must be rejected.
        for ws in [" \t\n ", "\r\u{3000}\t"] {
            assert!(
                matches!(
                    Token::new(ws.to_string()),
                    Err(AppError::Auth(AuthError::EmptyToken))
                ),
                "input {ws:?} should be EmptyToken"
            );
        }
    }

    #[test]
    fn empty_token_error_message_does_not_echo_rejected_input() {
        // OBSERVED BEHAVIOUR (documented, asserted): `Token::new` returns
        // the crate-wide `Result`, so the error surfaces as `AppError::Auth`
        // with the `auth: ` prefix, a fixed message, and never interpolates
        // the rejected input.
        for ws in ["", " ", "\t\n\r ", "\u{a0}"] {
            let err = Token::new(ws.to_string()).expect_err("whitespace-only");
            assert_eq!(err.to_string(), "auth: the API token must not be empty");
            let dbg = format!("{err:?}");
            assert_eq!(dbg, "Auth(EmptyToken)", "Debug echoed input: {dbg}");
        }
    }

    #[test]
    fn new_accepts_whitespace_around_real_content_and_preserves_it() {
        // OBSERVED BEHAVIOUR (documented, asserted): `Token::new` only
        // *validates* with trim; it does NOT normalise the stored value.
        // A token with surrounding whitespace around real content is
        // accepted and `expose()` returns the original, padding included.
        // Callers that want trimmed storage must trim before `new`.
        let padded = "  tok_fake_pad_77  ";
        let t = Token::new(padded.to_string()).expect("padded but non-empty");
        assert_eq!(t.expose(), padded);

        let tabbed = "\ttok_fake_tab_88\r\n";
        let t = Token::new(tabbed.to_string()).expect("tab-padded non-empty");
        assert_eq!(t.expose(), tabbed);
    }

    #[test]
    fn expose_returns_exact_original_for_various_values() {
        // Short boundary value, the acceptance fixture, marker-colliding
        // value, newlines inside, unicode, and a long value.
        let long = format!("tok_fake_{}", "x".repeat(512));
        for v in [
            "x",
            "tok_fake_abc123",
            "***",
            "tok***fake",
            "a\nb",
            long.as_str(),
        ] {
            let t = Token::new(v.to_string()).expect("non-empty value");
            assert_eq!(t.expose(), v);
            assert_eq!(t.expose().len(), v.len());
            // Repeated exposure is stable and does not mutate anything.
            assert_eq!(t.expose(), t.expose());
            // Formatting between exposures does not disturb the value.
            let _ = format!("{t}{t:?}");
            assert_eq!(t.expose(), v);
        }
    }

    #[test]
    fn display_and_debug_are_exactly_three_stars_for_edge_case_values() {
        // For every value (including a token that literally IS `***` and
        // ones containing `***`, plus realistic secret shapes) both
        // renderings are exactly the marker — same string, same length.
        let mut values: Vec<String> = FAKE_SECRETS.iter().map(|s| (*s).to_string()).collect();
        values.extend(
            [
                "x",
                "**",
                "***",
                "****",
                "tok***fake***tail",
                "a b",
                "\ttok_fake\n",
            ]
            .iter()
            .map(|s| (*s).to_string()),
        );
        values.push(format!("tok_fake_{}", "y".repeat(1024)));
        for v in &values {
            let t = Token::new(v.clone()).expect("non-empty value");
            let disp = format!("{t}");
            let dbg = format!("{t:?}");
            let pretty = format!("{t:#?}");
            assert_eq!(disp, "***", "Display for {v:?}");
            assert_eq!(dbg, "***", "Debug for {v:?}");
            assert_eq!(pretty, "***", "Debug-pretty for {v:?}");
            assert_eq!(disp.len(), 3, "marker length for {v:?}");
            assert_eq!(disp, crate::log::REDACTED);
            // The original must not be recoverable — except when the token
            // itself is a substring of the marker, which is unavoidable
            // for any fixed-width redaction (documented edge case).
            if !"***".contains(v.as_str()) {
                assert!(!disp.contains(v.as_str()), "Display leaked {v:?}");
                assert!(!dbg.contains(v.as_str()), "Debug leaked {v:?}");
            }
        }
    }

    #[test]
    fn token_stays_redacted_inside_nested_debug_structures() {
        // Debug delegation must hold when other types render the Token.
        let a = Token::new("tok_fake_nested_aa".to_string()).expect("non-empty");
        let b = Token::new("tok_fake_nested_bb".to_string()).expect("non-empty");
        assert_eq!(format!("{:?}", vec![&a, &b]), "[***, ***]");
        assert_eq!(format!("{:?}", Some(&a)), "Some(***)");
        assert_eq!(format!("token={a} kind=1"), "token=*** kind=1");
        let rendered = format!("{:?}", (&a, 7_u8, &b));
        assert_eq!(rendered, "(***, 7, ***)");
        assert!(!rendered.contains("tok_fake_nested"));
    }

    #[test]
    fn renderings_match_secret_bit_for_bit() {
        // Token must delegate to Secret's formatters, not reimplement
        // them: identical bytes for Display, Debug and pretty-Debug.
        for v in FAKE_SECRETS {
            let secret = Secret::new(v.to_string());
            let token = Token::new(v.to_string()).expect("non-empty");
            assert_eq!(format!("{token}"), format!("{secret}"));
            assert_eq!(format!("{token:?}"), format!("{secret:?}"));
            assert_eq!(format!("{token:#?}"), format!("{secret:#?}"));
        }
    }

    #[test]
    fn token_is_send_sync_static_for_async_wiring() {
        // Secret<String> holds a plain String, so Token is automatically
        // Send + Sync + 'static — assert it so later async/keyring tasks
        // can move Tokens across threads without surprises.
        fn assert_send_sync_static<T: Send + Sync + 'static>() {}
        assert_send_sync_static::<Token>();
        assert_send_sync_static::<Secret<String>>();
    }
}
