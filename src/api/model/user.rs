//! `User` resource shape from the Todoist Sync API (T-203, EPIC-03).
//!
//! Data only: no persistence, no business rules (EPIC-04 owns storage).
//! Unknown fields are accepted silently (no `deny_unknown_fields`) so
//! additive API changes deserialise without a code change, matching the
//! convention set by `common.rs` (T-202).
//!
//! The Sync API's `user` resource has no `is_deleted` field (accounts are
//! not soft-deleted the way projects/sections/labels/filters are), so
//! [`User`] intentionally omits it. It also has no `priority` field, so
//! [`super::Priority`] is intentionally not used here.

use serde::{Deserialize, Serialize};

use super::common::Id;

/// A Todoist user account, as returned by the Sync API's `user` resource.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct User {
    /// The user's unique id.
    pub id: Id,
    /// The user's full display name.
    pub full_name: String,
    /// The user's email address.
    #[serde(default)]
    pub email: String,
    /// The hex colour the Todoist UI uses to represent this user
    /// (e.g. avatar background), e.g. `"#ff8581"`.
    #[serde(default)]
    pub color: String,
    /// The user's timezone info, tolerant of unknown/missing nested fields.
    #[serde(default)]
    pub tz_info: TzInfo,
}

/// A user's timezone info, nested under [`User::tz_info`].
///
/// Every field is `#[serde(default)]` so a partial or empty `tz_info`
/// object (or one with fields this crate doesn't know about yet)
/// deserialises without error.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct TzInfo {
    /// The IANA timezone name, e.g. `"Europe/Lisbon"`.
    #[serde(default)]
    pub timezone: String,
    /// Hours offset from UTC.
    #[serde(default)]
    pub hours: i64,
    /// Minutes offset from UTC (in addition to `hours`).
    #[serde(default)]
    pub minutes: i64,
    /// Human-readable GMT offset string, e.g. `"GMT +00:00"`.
    #[serde(default)]
    pub gmt_string: String,
    /// Whether daylight saving time is currently in effect.
    #[serde(default)]
    pub is_dst: i64,
}

#[cfg(test)]
mod tests {
    //! Unit tests for the T-203 `User` / `TzInfo` resource shapes. All ids,
    //! names and email addresses are fake fixture data; no real personal
    //! data.
    use super::*;
    use serde_json::json;

    /// Compile-time proof that `User` and `TzInfo` derive the four traits
    /// the task requires (`Debug, Clone, Serialize, Deserialize`).
    fn assert_required_traits<
        T: std::fmt::Debug + Clone + Serialize + serde::de::DeserializeOwned,
    >() {
    }

    #[test]
    fn user_derives_debug_clone_serialize_deserialize() {
        assert_required_traits::<User>();
        assert_required_traits::<TzInfo>();

        let user: User = serde_json::from_value(json!({
            "id": "user-1",
            "full_name": "Ada Example"
        }))
        .unwrap();
        let cloned = user.clone();
        assert_eq!(cloned.full_name, "Ada Example");
        assert!(format!("{user:?}").contains("Ada Example"));
    }

    #[test]
    fn user_deserializes_id_full_name_colour_and_tz_info() {
        let user: User = serde_json::from_value(json!({
            "id": "user-1",
            "full_name": "Ada Example",
            "email": "ada@example.test",
            "color": "#ff8581",
            "tz_info": {
                "timezone": "Europe/Lisbon",
                "hours": 1,
                "minutes": 30,
                "gmt_string": "GMT +01:30",
                "is_dst": 1
            }
        }))
        .unwrap();

        assert_eq!(user.id.as_str(), "user-1");
        assert_eq!(user.full_name, "Ada Example");
        assert_eq!(user.email, "ada@example.test");
        assert_eq!(user.color, "#ff8581");
        assert_eq!(user.tz_info.timezone, "Europe/Lisbon");
        assert_eq!(user.tz_info.hours, 1);
        assert_eq!(user.tz_info.minutes, 30);
        assert_eq!(user.tz_info.gmt_string, "GMT +01:30");
        assert_eq!(user.tz_info.is_dst, 1);
    }

    #[test]
    fn user_applies_defaults_when_optional_fields_are_missing() {
        // Only `id` and `full_name` are mandatory.
        let user: User =
            serde_json::from_value(json!({ "id": "user-2", "full_name": "Fake Person" })).unwrap();

        assert_eq!(user.email, "");
        assert_eq!(user.color, "");
        assert_eq!(user.tz_info.timezone, "");
        assert_eq!(user.tz_info.hours, 0);
        assert_eq!(user.tz_info.minutes, 0);
        assert_eq!(user.tz_info.gmt_string, "");
        assert_eq!(user.tz_info.is_dst, 0);
    }

    #[test]
    fn user_accepts_a_partial_tz_info_object() {
        // The nested struct must stay tolerant of missing keys.
        let user: User = serde_json::from_value(json!({
            "id": "user-3",
            "full_name": "Fake Person",
            "tz_info": { "timezone": "UTC" }
        }))
        .unwrap();

        assert_eq!(user.tz_info.timezone, "UTC");
        assert_eq!(user.tz_info.hours, 0);
        assert_eq!(user.tz_info.gmt_string, "");
    }

    #[test]
    fn user_accepts_unknown_fields() {
        let user: User = serde_json::from_value(json!({
            "id": "user-4",
            "full_name": "Fake Person",
            "x": null
        }))
        .unwrap();

        assert_eq!(user.full_name, "Fake Person");
    }

    #[test]
    fn tz_info_accepts_unknown_fields() {
        // A new key inside the nested object must not break parsing either.
        let user: User = serde_json::from_value(json!({
            "id": "user-5",
            "full_name": "Fake Person",
            "tz_info": { "timezone": "UTC", "x": null }
        }))
        .unwrap();

        assert_eq!(user.tz_info.timezone, "UTC");
    }

    #[test]
    fn user_ignores_is_deleted_because_the_user_resource_has_no_such_field() {
        // The Sync API's `user` resource is not soft-deleted, so `User`
        // deliberately has no `is_deleted` field. Should the API ever start
        // sending one, it must be ignored rather than break parsing.
        let user: User = serde_json::from_value(json!({
            "id": "user-8",
            "full_name": "Fake Person",
            "is_deleted": 1
        }))
        .unwrap();

        assert_eq!(user.id.as_str(), "user-8");
    }

    #[test]
    fn user_round_trips_through_json() {
        let original = json!({
            "id": "user-6",
            "full_name": "Fake Person",
            "email": "fake@example.test",
            "color": "#4073ff",
            "tz_info": {
                "timezone": "America/Fake_City",
                "hours": -3,
                "minutes": 0,
                "gmt_string": "GMT -03:00",
                "is_dst": 0
            }
        });

        let user: User = serde_json::from_value(original.clone()).unwrap();
        assert_eq!(serde_json::to_value(&user).unwrap(), original);
    }

    #[test]
    fn user_rejects_a_missing_full_name() {
        let result: Result<User, _> = serde_json::from_value(json!({ "id": "user-7" }));

        assert!(result.is_err(), "missing `full_name` must not deserialise");
    }
}
