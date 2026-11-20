use std::collections::BTreeMap;
use std::fmt;
use std::str::FromStr;

use chrono::{DateTime, SecondsFormat, Utc};
use thiserror::Error;

use super::path::DocPath;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Timestamp(DateTime<Utc>);

impl Timestamp {
    #[must_use]
    pub fn new(at: DateTime<Utc>) -> Self {
        Self(at)
    }

    #[must_use]
    pub fn from_unix(seconds: i64, nanos: u32) -> Option<Self> {
        DateTime::from_timestamp(seconds, nanos).map(Self)
    }

    #[must_use]
    pub fn as_datetime(&self) -> DateTime<Utc> {
        self.0
    }

    #[must_use]
    pub fn seconds(&self) -> i64 {
        self.0.timestamp()
    }

    #[must_use]
    pub fn nanos(&self) -> u32 {
        self.0.timestamp_subsec_nanos()
    }
}

impl fmt::Display for Timestamp {
    /// RFC 3339 in UTC with 0, 3, 6 or 9 fractional digits, so the value round-trips exactly.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0.to_rfc3339_opts(SecondsFormat::AutoSi, true))
    }
}

impl FromStr for Timestamp {
    type Err = chrono::ParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        DateTime::parse_from_rfc3339(s).map(|t| Self(t.with_timezone(&Utc)))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct UpdateTime(Timestamp);

impl UpdateTime {
    #[must_use]
    pub fn new(at: Timestamp) -> Self {
        Self(at)
    }

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

#[derive(Debug, Clone, Copy, PartialEq, Error)]
pub enum GeoPointError {
    #[error("latitude {0} is out of range [-90, 90]")]
    Latitude(f64),
    #[error("longitude {0} is out of range [-180, 180]")]
    Longitude(f64),
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GeoPoint {
    lat: f64,
    lng: f64,
}

impl GeoPoint {
    pub fn new(lat: f64, lng: f64) -> Result<Self, GeoPointError> {
        if !(-90.0..=90.0).contains(&lat) {
            Err(GeoPointError::Latitude(lat))
        } else if !(-180.0..=180.0).contains(&lng) {
            Err(GeoPointError::Longitude(lng))
        } else {
            Ok(Self { lat, lng })
        }
    }

    #[must_use]
    pub fn lat(&self) -> f64 {
        self.lat
    }

    #[must_use]
    pub fn lng(&self) -> f64 {
        self.lng
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error(
    "`{0}` is not a full document resource name (projects/<project>/databases/<database>/documents/<collection>/<doc>)"
)]
pub struct ResourceNameError(String);

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct DocResourceName(String);

impl DocResourceName {
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for DocResourceName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl FromStr for DocResourceName {
    type Err = ResourceNameError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let parts = s
            .strip_prefix("projects/")
            .and_then(|rest| rest.split_once("/databases/"))
            .filter(|(project, _)| !project.is_empty() && !project.contains('/'))
            .and_then(|(_, rest)| rest.split_once("/documents/"))
            .filter(|(database, _)| !database.is_empty() && !database.contains('/'));
        parts
            .filter(|(_, path)| path.parse::<DocPath>().is_ok())
            .map(|_| Self(s.to_owned()))
            .ok_or_else(|| ResourceNameError(s.to_owned()))
    }
}

pub type Fields = BTreeMap<String, Value>;

#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Null,
    Bool(bool),
    Int(i64),
    Double(f64),
    String(String),
    Timestamp(Timestamp),
    Bytes(Vec<u8>),
    Ref(DocPath),
    ForeignRef(DocResourceName),
    Geo(GeoPoint),
    Array(Vec<Self>),
    Map(Fields),
}

impl From<&str> for Value {
    fn from(s: &str) -> Self {
        Self::String(s.to_owned())
    }
}

#[cfg(test)]
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

    #[test]
    fn resource_name_is_a_full_document_name() {
        for ok in [
            "projects/p/databases/(default)/documents/users/u1",
            "projects/p/databases/d/documents/users/u1/orders/o1",
        ] {
            let name: DocResourceName = ok.parse().unwrap();
            assert_eq!((name.as_str(), name.to_string()), (ok, ok.to_owned()));
        }
        for bad in [
            "",
            "users/u1",
            "projects/p/databases/d",
            "projects/p/databases/d/documents",
            "projects/p/databases/d/documents/users",
            "projects//databases/d/documents/users/u1",
            "projects/p/databases//documents/users/u1",
            "projects/p/q/databases/d/documents/users/u1",
            "projects/p/databases/d/documents/users//u1",
        ] {
            assert_eq!(
                bad.parse::<DocResourceName>(),
                Err(ResourceNameError(bad.to_owned())),
                "{bad}"
            );
        }
    }
}
