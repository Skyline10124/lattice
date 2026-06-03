//! Effective-boundary credit attribution.
//!
//! This module is Phase 28's compile-time/test-time guardrail: command
//! boundary credit must be attributed to the primitive that actually earns it.
//! Native code may add constructors for the evidence marker types only at the
//! corresponding Linux execution path.

use std::error::Error;
use std::fmt;

use lancet_protocol::{BoundaryDimension, BoundaryPrimitive, EffectiveBoundaryStatus};

/// Evidence that a command actually entered a no-egress network namespace.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NetnsApplied {
    _private: (),
}

/// Evidence that a command actually used an OverlayFS-backed view.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OverlayfsApplied {
    _private: (),
}

/// Evidence that a command actually entered the intended mount namespace.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MountNamespaceApplied {
    _private: (),
}

/// Evidence that Landlock was applied to the command.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LandlockApplied {
    _private: (),
}

/// Evidence variants allowed to contribute filesystem boundary credit.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FilesystemBoundaryEvidence {
    Overlayfs(OverlayfsApplied),
    MountNamespace(MountNamespaceApplied),
    Landlock(LandlockApplied),
}

impl FilesystemBoundaryEvidence {
    fn primitive(&self) -> BoundaryPrimitive {
        match self {
            FilesystemBoundaryEvidence::Overlayfs(_) => BoundaryPrimitive::Overlayfs,
            FilesystemBoundaryEvidence::MountNamespace(_) => BoundaryPrimitive::MountNamespace,
            FilesystemBoundaryEvidence::Landlock(_) => BoundaryPrimitive::Landlock,
        }
    }
}

/// A physically bounded credit claim after attribution validation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EffectiveBoundaryCredit {
    boundary: BoundaryDimension,
    source_primitive: BoundaryPrimitive,
    status: EffectiveBoundaryStatus,
}

impl EffectiveBoundaryCredit {
    /// Network physically-bounded credit is type-gated on netns evidence.
    ///
    /// No Landlock, overlay, or mount evidence type can be passed here.
    pub fn network_physically_bounded(evidence: NetnsApplied) -> Self {
        let _ = evidence;
        build_physically_bounded(
            BoundaryDimension::Network,
            BoundaryPrimitive::NetworkNamespace,
        )
        .expect("netns is the only allowed network boundary primitive")
    }

    /// Filesystem credit can come from several allowed primitives, but every
    /// claim still passes through the central attribution table.
    pub fn filesystem_physically_bounded(
        evidence: FilesystemBoundaryEvidence,
    ) -> Result<Self, BoundaryCreditError> {
        build_physically_bounded(BoundaryDimension::Filesystem, evidence.primitive())
    }

    /// Landlock is also exposed as its own boundary dimension when required.
    pub fn landlock_physically_bounded(evidence: LandlockApplied) -> Self {
        let _ = evidence;
        build_physically_bounded(BoundaryDimension::Landlock, BoundaryPrimitive::Landlock)
            .expect("landlock is the only allowed landlock boundary primitive")
    }

    pub fn boundary(&self) -> BoundaryDimension {
        self.boundary
    }

    pub fn source_primitive(&self) -> BoundaryPrimitive {
        self.source_primitive
    }

    pub fn status(&self) -> EffectiveBoundaryStatus {
        self.status
    }
}

/// Return the primitives allowed to earn physically-bounded credit for a
/// boundary dimension.
pub fn allowed_primitives_for(boundary: BoundaryDimension) -> &'static [BoundaryPrimitive] {
    match boundary {
        BoundaryDimension::Network => &[BoundaryPrimitive::NetworkNamespace],
        BoundaryDimension::Filesystem => &[
            BoundaryPrimitive::Overlayfs,
            BoundaryPrimitive::MountNamespace,
            BoundaryPrimitive::Landlock,
        ],
        BoundaryDimension::Landlock => &[BoundaryPrimitive::Landlock],
    }
}

/// Central attribution check used by all physically-bounded credit constructors.
pub fn validate_credit_attribution(
    boundary: BoundaryDimension,
    primitive: BoundaryPrimitive,
) -> Result<(), BoundaryCreditError> {
    if allowed_primitives_for(boundary).contains(&primitive) {
        Ok(())
    } else {
        Err(BoundaryCreditError {
            boundary,
            primitive,
        })
    }
}

fn build_physically_bounded(
    boundary: BoundaryDimension,
    source_primitive: BoundaryPrimitive,
) -> Result<EffectiveBoundaryCredit, BoundaryCreditError> {
    validate_credit_attribution(boundary, source_primitive)?;
    Ok(EffectiveBoundaryCredit {
        boundary,
        source_primitive,
        status: EffectiveBoundaryStatus::PhysicallyBounded,
    })
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BoundaryCreditError {
    pub boundary: BoundaryDimension,
    pub primitive: BoundaryPrimitive,
}

impl fmt::Display for BoundaryCreditError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} boundary cannot receive physically_bounded credit from {}",
            self.boundary.label(),
            self.primitive.label()
        )
    }
}

impl Error for BoundaryCreditError {}

/// ```compile_fail
/// use lancet_authority::{EffectiveBoundaryCredit, LandlockApplied};
///
/// let landlock: LandlockApplied = unsafe { std::mem::zeroed() };
/// let _ = EffectiveBoundaryCredit::network_physically_bounded(landlock);
/// ```
///
/// Network credit requires `NetnsApplied`; Landlock evidence is not accepted by
/// the type signature.
pub struct NetworkCreditRequiresNetnsEvidence;

#[cfg(test)]
mod tests {
    use super::*;

    impl NetnsApplied {
        fn for_test() -> Self {
            Self { _private: () }
        }
    }

    impl OverlayfsApplied {
        fn for_test() -> Self {
            Self { _private: () }
        }
    }

    impl MountNamespaceApplied {
        fn for_test() -> Self {
            Self { _private: () }
        }
    }

    impl LandlockApplied {
        fn for_test() -> Self {
            Self { _private: () }
        }
    }

    #[test]
    fn attribution_table_covers_every_boundary_dimension() {
        for boundary in BoundaryDimension::ALL {
            let allowed = allowed_primitives_for(*boundary);
            assert!(
                !allowed.is_empty(),
                "{} boundary must define at least one allowed primitive",
                boundary.label()
            );
        }
    }

    #[test]
    fn attribution_table_rejects_every_disallowed_primitive() {
        for boundary in BoundaryDimension::ALL {
            for primitive in BoundaryPrimitive::ALL {
                let expected = allowed_primitives_for(*boundary).contains(primitive);
                let actual = validate_credit_attribution(*boundary, *primitive).is_ok();
                assert_eq!(
                    actual,
                    expected,
                    "{} -> {} attribution mismatch",
                    boundary.label(),
                    primitive.label()
                );
            }
        }
    }

    #[test]
    fn landlock_never_earns_network_credit() {
        let err =
            validate_credit_attribution(BoundaryDimension::Network, BoundaryPrimitive::Landlock)
                .unwrap_err();
        assert_eq!(err.boundary, BoundaryDimension::Network);
        assert_eq!(err.primitive, BoundaryPrimitive::Landlock);
    }

    #[test]
    fn only_netns_earns_network_credit() {
        for primitive in BoundaryPrimitive::ALL {
            let result = validate_credit_attribution(BoundaryDimension::Network, *primitive);
            assert_eq!(
                result.is_ok(),
                *primitive == BoundaryPrimitive::NetworkNamespace,
                "{} must not earn network physically_bounded credit",
                primitive.label()
            );
        }
    }

    #[test]
    fn network_credit_requires_netns_evidence() {
        let credit = EffectiveBoundaryCredit::network_physically_bounded(NetnsApplied::for_test());
        assert_eq!(credit.boundary(), BoundaryDimension::Network);
        assert_eq!(
            credit.source_primitive(),
            BoundaryPrimitive::NetworkNamespace
        );
        assert_eq!(credit.status(), EffectiveBoundaryStatus::PhysicallyBounded);
    }

    #[test]
    fn filesystem_credit_accepts_only_attributed_primitives() {
        let overlay = EffectiveBoundaryCredit::filesystem_physically_bounded(
            FilesystemBoundaryEvidence::Overlayfs(OverlayfsApplied::for_test()),
        )
        .unwrap();
        assert_eq!(overlay.source_primitive(), BoundaryPrimitive::Overlayfs);

        let mount = EffectiveBoundaryCredit::filesystem_physically_bounded(
            FilesystemBoundaryEvidence::MountNamespace(MountNamespaceApplied::for_test()),
        )
        .unwrap();
        assert_eq!(mount.source_primitive(), BoundaryPrimitive::MountNamespace);

        let landlock = EffectiveBoundaryCredit::filesystem_physically_bounded(
            FilesystemBoundaryEvidence::Landlock(LandlockApplied::for_test()),
        )
        .unwrap();
        assert_eq!(landlock.source_primitive(), BoundaryPrimitive::Landlock);
    }

    #[test]
    fn landlock_boundary_credit_uses_landlock_primitive() {
        let credit =
            EffectiveBoundaryCredit::landlock_physically_bounded(LandlockApplied::for_test());
        assert_eq!(credit.boundary(), BoundaryDimension::Landlock);
        assert_eq!(credit.source_primitive(), BoundaryPrimitive::Landlock);
    }
}
