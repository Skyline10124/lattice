// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Alias of the single-file write byte cap for the residual hashline apply
//! engine.
//!
//! Live `mutate.create` / `mutate.edit` installs read
//! [`lancet_runtime::MAX_WORKSPACE_WRITE_BYTES`] through `host_mutate`. This
//! module keeps the same constant so the unused apply-engine tests cannot
//! drift from the production cap (#120 / #31 review).

use crate::executor::error::ToolError;

/// Maximum bytes a single mutation may write to one file.
const MAX_WRITE_SIZE: usize = lancet_runtime::MAX_WORKSPACE_WRITE_BYTES;

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
        assert_eq!(
            WriteLimits::default().max_write_size,
            lancet_runtime::MAX_WORKSPACE_WRITE_BYTES
        );
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
