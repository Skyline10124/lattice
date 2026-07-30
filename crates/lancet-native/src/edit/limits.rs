// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! The single-file write byte cap shared by every workspace write path.
//!
//! One definition, read by both Atomic Local Mutation paths: the
//! `mutate.create` install preflight and the HashLine apply update stage.
//! It is deliberately not a profile/config axis — a Cell must not be able to
//! raise its own write budget.

use crate::executor::error::ToolError;

/// Maximum bytes a single mutation may write to one file.
const MAX_WRITE_SIZE: usize = 1024 * 1024;

/// Byte budget for one installed file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WriteLimits {
    pub(crate) max_write_size: usize,
}

impl Default for WriteLimits {
    fn default() -> Self {
        Self {
            max_write_size: MAX_WRITE_SIZE,
        }
    }
}

impl WriteLimits {
    /// Reject content whose byte length exceeds the cap.
    ///
    /// The error carries [`ToolError::SizeLimit`], so every write path
    /// surfaces the same limit/actual text to the client.
    pub(crate) fn check_write_size(&self, actual: usize) -> Result<(), ToolError> {
        if actual > self.max_write_size {
            return Err(ToolError::SizeLimit {
                limit: self.max_write_size,
                actual,
            });
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_limits_default_is_one_mib() {
        assert_eq!(WriteLimits::default().max_write_size, 1024 * 1024);
    }

    #[test]
    fn check_write_size_accepts_the_boundary_and_rejects_one_byte_more() {
        let limits = WriteLimits::default();
        let cap = limits.max_write_size;
        assert!(limits.check_write_size(cap).is_ok());
        let error = limits.check_write_size(cap + 1).expect_err("over cap");
        assert_eq!(
            error.to_string(),
            format!("size limit exceeded: {} > {cap}", cap + 1)
        );
    }
}
