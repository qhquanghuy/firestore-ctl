use super::path::DocPath;
use super::value::{Fields, UpdateTime};

#[derive(Debug, Clone, PartialEq)]
pub struct Document {
    pub path: DocPath,
    pub update_time: UpdateTime,
    pub data: Fields,
}
