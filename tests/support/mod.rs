#![expect(
    clippy::expect_used,
    clippy::indexing_slicing,
    reason = "test code fails by panicking"
)]

pub(crate) mod fake;
