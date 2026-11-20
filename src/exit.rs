use std::process::ExitCode;

use crate::app::AppError;
use crate::port::GatewayError;

#[must_use]
pub fn exit_code(e: &AppError) -> ExitCode {
    ExitCode::from(exit_code_value(e))
}

/// Exit 3 is a missing *document*; a missing database or project ([`GatewayError::ResourceNotFound`]) is
/// an ordinary failure (1).
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
