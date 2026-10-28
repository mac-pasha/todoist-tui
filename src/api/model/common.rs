//! Scalar types shared by every Todoist Sync API resource (T-202, EPIC-03).
//!
//! * [`Id`] — the `String`-backed identifier newtype used by every later
//!   model task (project, section, item, ...).
//! * [`Due`] / [`Deadline`] — the `due` and `deadline` objects embedded in
//!   items and other resources. Per the epic's drift-risk note, every field
//!   beyond the mandatory `date` is `Option` with `#[serde(default)]`, and
//!   no struct here sets `deny_unknown_fields`, so additive API changes
//!   (new keys, newly-optional keys) deserialise without a code change.
//! * [`Priority`] — the API's `1..=4` priority integer, validated on
//!   deserialise.
//!
//! Timestamps stay plain `String`: the crate has no `chrono` dependency,
//! and parsing the `due`/`deadline` date forms into real dates is a later
//! epic's concern (see module docs above).

use std::fmt;

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use thiserror::Error;

/// A Todoist Sync API object identifier.
///
/// The Sync API represents every id (project, section, item, ...) as an
/// opaque string, so `Id` wraps `String` directly rather than parsing it
/// into a numeric type. Constructible from any `Into<String>`; the inner
/// value is accessed via [`Id::as_str`] or [`AsRef<str>`].
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Id(String);

impl Id {
    /// Builds an `Id` from an owned or borrowed string.
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// Borrows the raw id string.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for Id {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl AsRef<str> for Id {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl From<String> for Id {
    fn from(value: String) -> Self {
        Self(value)
    }
}

impl From<&str> for Id {
    fn from(value: &str) -> Self {
        Self(value.to_owned())
    }
}

/// The Sync API `due` object, embedded in items (and other resources) that
/// carry a due date.
///
/// `date` is kept as the raw API string and accepts, unparsed, any of the
/// Sync API's date forms (date-only `2024-05-01`, floating datetime
/// `2024-05-01T10:00:00`, or UTC datetime `2024-05-01T10:00:00Z`) so the
/// original value round-trips losslessly through (de)serialisation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Due {
    /// Raw due date string, in one of the Sync API's supported forms.
    pub date: String,

    /// Human-readable rendering of the due date (e.g. `"every day"`), when
    /// the API provides one.
    #[serde(default)]
    pub string: Option<String>,

    /// Language code for `string`, when the API provides one.
    #[serde(default)]
    pub lang: Option<String>,

    /// Whether this due date recurs. Absent/`null` is treated as `false`.
    #[serde(default)]
    pub is_recurring: bool,

    /// IANA timezone name; present for fixed due dates, absent for
    /// floating ones.
    #[serde(default)]
    pub timezone: Option<String>,
}

/// The Sync API `deadline` object, embedded in items that carry a
/// deadline separate from their `due` date.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Deadline {
    /// Raw deadline date string (date-only form per the Sync API).
    pub date: String,

    /// Language code for the deadline's display text, when the API
    /// provides one.
    #[serde(default)]
    pub lang: Option<String>,
}

/// A validated Todoist task priority.
///
/// The Sync API encodes priority as an integer in `1..=4` (`4` is the
/// highest/"urgent" priority). [`Priority`] rejects any other integer at
/// deserialisation time rather than silently accepting it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Priority(u8);

impl Priority {
    /// Lowest valid priority value accepted by the Sync API.
    pub const MIN: u8 = 1;
    /// Highest valid priority value accepted by the Sync API.
    pub const MAX: u8 = 4;

    /// Builds a `Priority` from a raw integer.
    ///
    /// # Errors
    ///
    /// Returns [`PriorityOutOfRange`] when `value` is outside `1..=4`.
    pub fn new(value: u8) -> Result<Self, PriorityOutOfRange> {
        match value {
            Self::MIN..=Self::MAX => Ok(Self(value)),
            _ => Err(PriorityOutOfRange { value }),
        }
    }

    /// Returns the raw `1..=4` integer value.
    #[must_use]
    pub const fn get(self) -> u8 {
        self.0
    }
}

impl TryFrom<u8> for Priority {
    type Error = PriorityOutOfRange;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<Priority> for u8 {
    fn from(priority: Priority) -> Self {
        priority.0
    }
}

impl<'de> Deserialize<'de> for Priority {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = u8::deserialize(deserializer)?;
        Priority::new(value).map_err(serde::de::Error::custom)
    }
}

impl Serialize for Priority {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_u8(self.0)
    }
}

/// A priority integer outside the Sync API's valid `1..=4` range.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
#[error(
    "priority must be between {} and {} (got {value})",
    Priority::MIN,
    Priority::MAX
)]
pub struct PriorityOutOfRange {
    /// The out-of-range value that was rejected.
    pub value: u8,
}

#[cfg(test)]
mod tests {
    //! Unit tests for the T-202 shared sync scalar types. All data is
    //! fake fixture data; no real credentials or personal data.
    use super::*;
    use serde_json::json;

    // --- Due -----------------------------------------------------------

    #[test]
    fn due_round_trips_all_five_fields() {
        let original = json!({
            "date": "2024-05-01T10:00:00Z",
            "string": "May 1",
            "lang": "en",
            "is_recurring": true,
            "timezone": "Europe/Berlin"
        });
        let due: Due = serde_json::from_value(original.clone()).unwrap();
        assert_eq!(due.date, "2024-05-01T10:00:00Z");
        assert_eq!(due.string.as_deref(), Some("May 1"));
        assert_eq!(due.lang.as_deref(), Some("en"));
        assert!(due.is_recurring);
        assert_eq!(due.timezone.as_deref(), Some("Europe/Berlin"));

        let reparsed = serde_json::to_value(&due).unwrap();
        assert_eq!(reparsed, original);
    }

    #[test]
    fn due_applies_defaults_to_optional_fields() {
        let due: Due = serde_json::from_value(json!({ "date": "2024-05-01" })).unwrap();
        assert_eq!(due.date, "2024-05-01");
        assert_eq!(due.string, None);
        assert_eq!(due.lang, None);
        assert!(!due.is_recurring);
        assert_eq!(due.timezone, None);
    }

    #[test]
    fn due_treats_null_optional_fields_as_absent() {
        // NOTE: `is_recurring` is intentionally omitted here. Explicit JSON
        // `null` is NOT tolerated for it — see test below.
        let due: Due = serde_json::from_value(json!({
            "date": "2024-05-01",
            "string": null,
            "lang": null,
            "is_recurring": true,
            "timezone": null
        }))
        .unwrap();
        assert_eq!(due.string, None);
        assert_eq!(due.lang, None);
        assert!(due.is_recurring);
        assert_eq!(due.timezone, None);
    }

    #[test]
    fn due_rejects_explicit_null_is_recurring() {
        // BUG (T-202): the doc comment claims "Absent/`null` is treated as
        // `false`", but `#[serde(default)]` on a bare `bool` only covers the
        // absent case; an explicit `null` is a decode error:
        //   Error("invalid type: null, expected a boolean")
        let err = serde_json::from_value::<Due>(json!({
            "date": "2024-05-01",
            "is_recurring": null
        }))
        .unwrap_err();
        assert!(
            err.to_string().contains("boolean"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn due_rejects_missing_mandatory_date() {
        let err = serde_json::from_value::<Due>(json!({
            "string": "every day",
            "is_recurring": true
        }))
        .unwrap_err();
        assert!(
            err.to_string().contains("date"),
            "error should name `date`: {err}"
        );
    }

    // --- Due date forms -------------------------------------------------

    #[test]
    fn due_date_only_form_round_trips_byte_exact() {
        assert_date_round_trips("2024-05-01");
    }

    #[test]
    fn due_floating_datetime_form_round_trips_byte_exact() {
        assert_date_round_trips("2024-05-01T10:00:00");
    }

    #[test]
    fn due_utc_datetime_form_round_trips_byte_exact() {
        assert_date_round_trips("2024-05-01T10:00:00Z");
    }

    fn assert_date_round_trips(date: &str) {
        let due: Due = serde_json::from_value(json!({ "date": date })).unwrap();
        assert_eq!(due.date, date);
        let reparsed = serde_json::to_value(&due).unwrap();
        assert_eq!(reparsed["date"], date);
        assert_eq!(serde_json::from_value::<Due>(reparsed).unwrap().date, date);
    }

    // --- Unknown-field tolerance -----------------------------------------

    #[test]
    fn due_tolerates_unknown_fields() {
        let due: Due = serde_json::from_value(json!({
            "date": "2024-05-01",
            "future_field": 1
        }))
        .unwrap();
        assert_eq!(due.date, "2024-05-01");
    }

    #[test]
    fn deadline_tolerates_unknown_fields() {
        let deadline: Deadline = serde_json::from_value(json!({
            "date": "2024-06-01",
            "future_field": 1
        }))
        .unwrap();
        assert_eq!(deadline.date, "2024-06-01");
    }

    // --- Deadline --------------------------------------------------------

    #[test]
    fn deadline_round_trips_all_fields() {
        let original = json!({
            "date": "2024-06-01",
            "lang": "en"
        });
        let deadline: Deadline = serde_json::from_value(original.clone()).unwrap();
        assert_eq!(deadline.date, "2024-06-01");
        assert_eq!(deadline.lang.as_deref(), Some("en"));
        assert_eq!(serde_json::to_value(&deadline).unwrap(), original);
    }

    #[test]
    fn deadline_applies_default_to_lang() {
        let deadline: Deadline = serde_json::from_value(json!({ "date": "2024-06-01" })).unwrap();
        assert_eq!(deadline.lang, None);
    }

    // --- Priority ---------------------------------------------------------

    #[test]
    fn priority_accepts_every_valid_value() {
        for value in 1u8..=4 {
            let priority: Priority = serde_json::from_value(json!(value))
                .unwrap_or_else(|err| panic!("priority {value} should deserialise: {err}"));
            assert_eq!(u8::from(priority), value);
            assert_eq!(
                serde_json::to_value(priority).unwrap(),
                json!(value),
                "priority {value} must serialise back to the same integer"
            );
        }
    }

    #[test]
    fn priority_rejects_out_of_range_values_with_decode_error() {
        for value in [0i64, 5, 6, 128, 255] {
            match serde_json::from_value::<Priority>(json!(value)) {
                Ok(unexpected) => panic!("priority {value} should be rejected, got {unexpected:?}"),
                Err(err) => assert!(
                    err.to_string().contains(&value.to_string()),
                    "error should mention the offending value {value}: {err}"
                ),
            }
        }
    }

    #[test]
    fn priority_rejects_negative_values() {
        let err = serde_json::from_value::<Priority>(json!(-1)).unwrap_err();
        assert!(
            err.to_string().contains("-1"),
            "error should mention -1: {err}"
        );
    }

    #[test]
    fn priority_rejects_non_integer_values() {
        assert!(serde_json::from_value::<Priority>(json!("high")).is_err());
        assert!(serde_json::from_value::<Priority>(json!(2.5)).is_err());
    }

    #[test]
    fn priority_new_and_try_from_match_range() {
        for value in 1u8..=4 {
            assert!(Priority::new(value).is_ok());
            assert_eq!(
                Priority::try_from(value).unwrap(),
                Priority::new(value).unwrap()
            );
        }
        assert!(matches!(
            Priority::new(0),
            Err(PriorityOutOfRange { value: 0 })
        ));
        assert!(matches!(
            Priority::new(5),
            Err(PriorityOutOfRange { value: 5 })
        ));
    }

    #[test]
    fn priority_bounds_constants_are_api_range() {
        assert_eq!(Priority::MIN, 1);
        assert_eq!(Priority::MAX, 4);
    }

    // --- Id ---------------------------------------------------------------

    #[test]
    fn id_is_string_backed_and_transparent() {
        let id: Id = serde_json::from_value(json!("proj_abc_123")).unwrap();
        assert_eq!(id.as_str(), "proj_abc_123");
        assert_eq!(id.to_string(), "proj_abc_123");
        assert_eq!(id.as_ref(), "proj_abc_123");
        // `#[serde(transparent)]`: serialises as a bare JSON string, not an
        // object, so later resource models embed it plainly.
        assert_eq!(serde_json::to_value(&id).unwrap(), json!("proj_abc_123"));
    }

    #[test]
    fn id_new_and_from_conversions() {
        assert_eq!(Id::new("x_1".to_string()).as_str(), "x_1");
        assert_eq!(Id::from("x_2".to_string()).as_str(), "x_2");
        assert_eq!(Id::from("x_3").as_str(), "x_3");
    }
}
