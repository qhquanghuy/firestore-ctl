//! Exit codes (documented in `--help`):
//! `0` ok, `1` other error, `2` usage (clap), `3` document not found, `4` already exists,
//! `5` precondition failed, `6` writes disabled, `7` missing index, `8` auth.

use std::process::ExitCode;

use crate::app::AppError;
use crate::port::GatewayError;

#[must_use]
pub fn exit_code(e: &AppError) -> ExitCode {
    ExitCode::from(exit_code_value(e))
}

/// Exit 3 means a *document* was not found; a missing
/// database or project ([`GatewayError::ResourceNotFound`]) is an ordinary failure (1).
#[must_use]
pub fn exit_code_value(e: &AppError) -> u8 {
    match e {
        AppError::Input(_) => 2,
        AppError::NotFound(_) | AppError::Gateway(GatewayError::NotFound(_)) => 3,
        AppError::Gateway(GatewayError::AlreadyExists(_)) => 4,
        AppError::Gateway(GatewayError::PreconditionFailed(_)) => 5,
        AppError::WritesDisabled(_) => 6,
        AppError::MissingIndex { .. } | AppError::Gateway(GatewayError::MissingIndex { .. }) => 7,
        AppError::Gateway(GatewayError::Auth(_) | GatewayError::PermissionDenied(_)) => 8,
        AppError::Gateway(GatewayError::ResourceNotFound(_) | GatewayError::Other(_))
        | AppError::Policy(_) => 1,
    }
}
