//! Firestore values: the typed data model shared by parsing, codecs and rendering.

use std::collections::BTreeMap;
use std::fmt;
use std::str::FromStr;

use chrono::{DateTime, SecondsFormat, Utc};
use thiserror::Error;

use super::path::DocPath;

/// A point in time with nanosecond precision (what Firestore stores).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Timestamp(DateTime<Utc>);

impl Timestamp {
    /// Wraps a UTC datetime without losing precision.
    #[must_use]
    pub fn new(at: DateTime<Utc>) -> Self {
        Self(at)
    }

    /// Builds from whole seconds since the Unix epoch plus nanoseconds
    /// (`0..=999_999_999`, as in the Firestore protos); `None` if out of range.
    #[must_use]
    pub fn from_unix(seconds: i64, nanos: u32) -> Option<Self> {
        DateTime::from_timestamp(seconds, nanos).map(Self)
    }

    /// The underlying UTC datetime.
    #[must_use]
    pub fn as_datetime(&self) -> DateTime<Utc> {
        self.0
    }

    /// Whole seconds since the Unix epoch.
    #[must_use]
    pub fn seconds(&self) -> i64 {
        self.0.timestamp()
    }

    /// Nanoseconds past the whole second.
    #[must_use]
    pub fn nanos(&self) -> u32 {
        self.0.timestamp_subsec_nanos()
    }
}

impl fmt::Display for Timestamp {
    /// RFC 3339 in UTC (`Z`), with as many fractional digits as needed
    /// (0, 3, 6 or 9) so the value round-trips exactly.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0.to_rfc3339_opts(SecondsFormat::AutoSi, true))
    }
}

impl FromStr for Timestamp {
    type Err = chrono::ParseError;

    /// Parses RFC 3339 (any offset), normalising to UTC.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        DateTime::parse_from_rfc3339(s).map(|t| Self(t.with_timezone(&Utc)))
    }
}

/// A document's last-modified time; also the precondition value of guarded writes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct UpdateTime(Timestamp);

impl UpdateTime {
    /// Wraps a timestamp as an update time.
    #[must_use]
    pub fn new(at: Timestamp) -> Self {
        Self(at)
    }

    /// The wrapped timestamp.
    #[must_use]
    pub fn timestamp(&self) -> Timestamp {
        self.0
    }
}

impl fmt::Display for UpdateTime {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl FromStr for UpdateTime {
    type Err = chrono::ParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        s.parse().map(Self)
    }
}

/// Why a latitude/longitude pair is not a valid [`GeoPoint`].
#[derive(Debug, Clone, Copy, PartialEq, Error)]
pub enum GeoPointError {
    /// Latitude outside `[-90, 90]` (or NaN).
    #[error("latitude {0} is out of range [-90, 90]")]
    Latitude(f64),
    /// Longitude outside `[-180, 180]` (or NaN).
    #[error("longitude {0} is out of range [-180, 180]")]
    Longitude(f64),
}

/// A geographic point with validated coordinates.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GeoPoint {
    lat: f64,
    lng: f64,
}

impl GeoPoint {
    /// # Errors
    /// [`GeoPointError`] if `lat` is outside `[-90, 90]` or `lng` outside `[-180, 180]`.
    pub fn new(lat: f64, lng: f64) -> Result<Self, GeoPointError> {
        if !(-90.0..=90.0).contains(&lat) {
            Err(GeoPointError::Latitude(lat))
        } else if !(-180.0..=180.0).contains(&lng) {
            Err(GeoPointError::Longitude(lng))
        } else {
            Ok(Self { lat, lng })
        }
    }

    /// Latitude in degrees.
    #[must_use]
    pub fn lat(&self) -> f64 {
        self.lat
    }

    /// Longitude in degrees.
    #[must_use]
    pub fn lng(&self) -> f64 {
        self.lng
    }
}

/// The fields of a document or map value, ordered by key.
pub type Fields = BTreeMap<String, Value>;

/// One Firestore value.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    /// `null`.
    Null,
    /// A boolean.
    Bool(bool),
    /// A 64-bit integer.
    Int(i64),
    /// A double-precision float.
    Double(f64),
    /// A UTF-8 string.
    String(String),
    /// A timestamp.
    Timestamp(Timestamp),
    /// Raw bytes.
    Bytes(Vec<u8>),
    /// A reference to another document.
    Ref(DocPath),
    /// A geographic point.
    Geo(GeoPoint),
    /// An ordered list of values.
    Array(Vec<Value>),
    /// A nested map.
    Map(Fields),
}

impl From<&str> for Value {
    fn from(s: &str) -> Self {
        Self::String(s.to_owned())
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn timestamp_keeps_nanoseconds() {
        let t: Timestamp = "2026-01-05T10:00:00.123456789Z".parse().unwrap();
        assert_eq!(t.nanos(), 123_456_789);
        assert_eq!(t.to_string(), "2026-01-05T10:00:00.123456789Z");
        assert_eq!(Timestamp::from_unix(t.seconds(), t.nanos()), Some(t));
    }

    #[test]
    fn timestamp_normalises_offsets_to_utc() {
        let t: Timestamp = "2026-01-05T11:00:00+01:00".parse().unwrap();
        assert_eq!(t.to_string(), "2026-01-05T10:00:00Z");
    }

    #[test]
    fn geo_point_validates_ranges() {
        assert!(GeoPoint::new(90.0, -180.0).is_ok());
        assert_eq!(GeoPoint::new(90.5, 0.0), Err(GeoPointError::Latitude(90.5)));
        assert_eq!(
            GeoPoint::new(0.0, 181.0),
            Err(GeoPointError::Longitude(181.0))
        );
        assert!(GeoPoint::new(f64::NAN, 0.0).is_err());
        let p = GeoPoint::new(35.5, 139.7).unwrap();
        assert_eq!((p.lat(), p.lng()), (35.5, 139.7));
    }
}
