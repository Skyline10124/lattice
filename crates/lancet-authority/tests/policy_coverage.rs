//! Capability coverage matrix for all catalog methods.
//!
//! These tests ensure that every catalog method has explicit capability
//! coverage and that adding a new method without coverage fails the build.

use lancet_catalog::method::RequiredCapability;
use lancet_protocol::envelope::{BaseExecutionEnvelope, Capability};
use lancet_protocol::policy::{CapabilitySource, SourcedCapability};

use lancet_authority::policy::capability::{
    match_capability, CapabilityMatchVerdict, InvocationFacts, SessionStateOperation,
};
use lancet_authority::policy::default::CapabilitySourcePolicy;

/// Fixture row describing expected capability behavior for a catalog method.
struct CapabilityFixture {
    method: &'static str,
    required_capability: RequiredCapability,
    invocation: InvocationFacts,
    /// Expected verdict when using only a Base-sourced capability.
    base_verdict: CapabilityMatchVerdict,
    /// Expected verdict when using a Declared-sourced capability.
    declared_verdict: CapabilityMatchVerdict,
}

fn base_source() -> CapabilitySource {
    CapabilitySource::Base
}

fn declared_source() -> CapabilitySource {
    CapabilitySource::Declared {
        envelope_id: "test-envelope".into(),
    }
}

/// Build the capability fixture matrix covering all 10 catalog methods.
///
/// Each row maps a method to its RequiredCapability, representative invocation
/// facts, and expected verdicts for Base-only and Declared-only sources.
fn capability_fixtures() -> Vec<CapabilityFixture> {
    vec![
        // workspace.read_file → ReadPath
        CapabilityFixture {
            method: "workspace.read_file",
            required_capability: RequiredCapability::ReadPath,
            invocation: InvocationFacts {
                path: Some("src/main.rs".into()),
                ..Default::default()
            },
            base_verdict: CapabilityMatchVerdict::Satisfied,
            declared_verdict: CapabilityMatchVerdict::Satisfied,
        },
        // workspace.grep → Inspect
        CapabilityFixture {
            method: "workspace.grep",
            required_capability: RequiredCapability::Inspect,
            invocation: InvocationFacts {
                path: Some("src/".into()),
                ..Default::default()
            },
            base_verdict: CapabilityMatchVerdict::Satisfied,
            declared_verdict: CapabilityMatchVerdict::Satisfied,
        },
        // workspace.list_directory → Inspect
        CapabilityFixture {
            method: "workspace.list_directory",
            required_capability: RequiredCapability::Inspect,
            invocation: InvocationFacts {
                path: Some("src/".into()),
                ..Default::default()
            },
            base_verdict: CapabilityMatchVerdict::Satisfied,
            declared_verdict: CapabilityMatchVerdict::Satisfied,
        },
        // workspace.find_files → Inspect
        CapabilityFixture {
            method: "workspace.find_files",
            required_capability: RequiredCapability::Inspect,
            invocation: InvocationFacts {
                path: Some("src/".into()),
                ..Default::default()
            },
            base_verdict: CapabilityMatchVerdict::Satisfied,
            declared_verdict: CapabilityMatchVerdict::Satisfied,
        },
        // web.fetch → FetchUrl
        CapabilityFixture {
            method: "web.fetch",
            required_capability: RequiredCapability::FetchUrl,
            invocation: InvocationFacts {
                host: Some("example.com".into()),
                ..Default::default()
            },
            base_verdict: CapabilityMatchVerdict::Satisfied,
            declared_verdict: CapabilityMatchVerdict::Satisfied,
        },
        // session.todo_write → MutateSessionState
        CapabilityFixture {
            method: "session.todo_write",
            required_capability: RequiredCapability::MutateSessionState,
            invocation: InvocationFacts {
                session_state_operation: Some(SessionStateOperation::TodoWrite),
                ..Default::default()
            },
            base_verdict: CapabilityMatchVerdict::Satisfied,
            declared_verdict: CapabilityMatchVerdict::Satisfied,
        },
        // command.run → RunCommand
        CapabilityFixture {
            method: "command.run",
            required_capability: RequiredCapability::RunCommand,
            invocation: InvocationFacts {
                argv: Some(vec!["cargo".into(), "test".into()]),
                ..Default::default()
            },
            // Base-sourced RunCommand is SourceNotAllowed for command.run.
            base_verdict: CapabilityMatchVerdict::SourceNotAllowed,
            declared_verdict: CapabilityMatchVerdict::Satisfied,
        },
        // workspace.write_file → WritePath
        CapabilityFixture {
            method: "workspace.write_file",
            required_capability: RequiredCapability::WritePath,
            invocation: InvocationFacts {
                path: Some("src/main.rs".into()),
                ..Default::default()
            },
            // Base-sourced WritePath is SourceNotAllowed for mutation.
            base_verdict: CapabilityMatchVerdict::SourceNotAllowed,
            declared_verdict: CapabilityMatchVerdict::Satisfied,
        },
        // workspace.patch → WritePath
        CapabilityFixture {
            method: "workspace.patch",
            required_capability: RequiredCapability::WritePath,
            invocation: InvocationFacts {
                path: Some("src/main.rs".into()),
                ..Default::default()
            },
            base_verdict: CapabilityMatchVerdict::SourceNotAllowed,
            declared_verdict: CapabilityMatchVerdict::Satisfied,
        },
        // envelope.declare → MutateSessionState
        CapabilityFixture {
            method: "envelope.declare",
            required_capability: RequiredCapability::MutateSessionState,
            invocation: InvocationFacts {
                session_state_operation: Some(SessionStateOperation::EnvelopeDeclare),
                ..Default::default()
            },
            base_verdict: CapabilityMatchVerdict::Satisfied,
            declared_verdict: CapabilityMatchVerdict::Satisfied,
        },
        // envelope.submit_intent uses MutateSessionState.
        CapabilityFixture {
            method: "envelope.submit_intent",
            required_capability: RequiredCapability::MutateSessionState,
            invocation: InvocationFacts {
                session_state_operation: Some(SessionStateOperation::SubmitIntent),
                ..Default::default()
            },
            base_verdict: CapabilityMatchVerdict::Satisfied,
            declared_verdict: CapabilityMatchVerdict::Satisfied,
        },
        // audit.query → ReadAudit (session-scoped, pattern-less)
        CapabilityFixture {
            method: "audit.query",
            required_capability: RequiredCapability::ReadAudit,
            invocation: InvocationFacts::default(),
            base_verdict: CapabilityMatchVerdict::Satisfied,
            declared_verdict: CapabilityMatchVerdict::Satisfied,
        },
        // audit.replay → ReadAudit
        CapabilityFixture {
            method: "audit.replay",
            required_capability: RequiredCapability::ReadAudit,
            invocation: InvocationFacts::default(),
            base_verdict: CapabilityMatchVerdict::Satisfied,
            declared_verdict: CapabilityMatchVerdict::Satisfied,
        },
        // audit.tail → ReadAudit (session-scoped streaming read)
        CapabilityFixture {
            method: "audit.tail",
            required_capability: RequiredCapability::ReadAudit,
            invocation: InvocationFacts::default(),
            base_verdict: CapabilityMatchVerdict::Satisfied,
            declared_verdict: CapabilityMatchVerdict::Satisfied,
        },
    ]
}

/// Build a SourcedCapability that matches the fixture's required capability.
fn sourced_cap_for(required: RequiredCapability, source: CapabilitySource) -> SourcedCapability {
    let capability = match required {
        RequiredCapability::ReadPath => Capability::ReadPath {
            path_pattern: "src".into(),
        },
        RequiredCapability::WritePath => Capability::WritePath {
            path_pattern: "src".into(),
        },
        RequiredCapability::Inspect => Capability::Inspect,
        RequiredCapability::RunCommand => Capability::RunCommand {
            argv_allowlist: vec!["cargo test".into()],
        },
        RequiredCapability::FetchUrl => Capability::FetchUrl {
            host: "example.com".into(),
        },
        RequiredCapability::MutateSessionState => Capability::MutateSessionState,
        RequiredCapability::ReadAudit => Capability::ReadAudit,
    };
    SourcedCapability { capability, source }
}

// ── Capability coverage tests ──────────────────────────────────────────

/// Verify every catalog method has an explicit fixture row.
///
/// If a new method is added to the catalog without a fixture row, this test
/// fails with the missing method names listed.
#[test]
fn all_catalog_methods_have_capability_coverage() {
    let catalog = lancet_catalog::catalog::catalog();
    let fixtures = capability_fixtures();
    let fixture_methods: Vec<&str> = fixtures.iter().map(|f| f.method).collect();

    let mut missing = Vec::new();
    for descriptor in catalog {
        if !fixture_methods.contains(&descriptor.name) {
            missing.push(descriptor.name);
        }
    }

    assert!(
        missing.is_empty(),
        "Catalog methods missing capability coverage fixtures: {:?}\n\
         Add a row to capability_fixtures() for each missing method.",
        missing
    );
}

/// For each fixture row, verify the fixture's required_capability matches the
/// catalog descriptor, then run match_capability with Base-only and Declared-only
/// snapshots and assert the typed verdicts.
#[test]
fn capability_matrix_covers_all_methods() {
    let catalog = lancet_catalog::catalog::catalog();
    let fixtures = capability_fixtures();

    for fixture in &fixtures {
        let descriptor = catalog
            .iter()
            .find(|d| d.name == fixture.method)
            .unwrap_or_else(|| panic!("Method '{}' not found in catalog", fixture.method));

        assert_eq!(
            descriptor.capability, fixture.required_capability,
            "RequiredCapability mismatch for {}: catalog has {:?}, fixture expects {:?}",
            fixture.method, descriptor.capability, fixture.required_capability
        );

        // Test with Base-only source
        let base_caps = vec![sourced_cap_for(fixture.required_capability, base_source())];
        let base_result = match_capability(
            fixture.required_capability,
            &fixture.invocation,
            &base_caps,
            fixture.method,
            &CapabilitySourcePolicy::default(),
        );
        assert_eq!(
            base_result.verdict, fixture.base_verdict,
            "Base verdict mismatch for {}: expected {:?}, got {:?}",
            fixture.method, fixture.base_verdict, base_result.verdict
        );

        // Test with Declared-only source
        let declared_caps = vec![sourced_cap_for(
            fixture.required_capability,
            declared_source(),
        )];
        let declared_result = match_capability(
            fixture.required_capability,
            &fixture.invocation,
            &declared_caps,
            fixture.method,
            &CapabilitySourcePolicy::default(),
        );
        assert_eq!(
            declared_result.verdict, fixture.declared_verdict,
            "Declared verdict mismatch for {}: expected {:?}, got {:?}",
            fixture.method, fixture.declared_verdict, declared_result.verdict
        );
    }
}

/// Verify that `BaseExecutionEnvelope` never grants `WritePath` or `RunCommand`
/// This also validates the command source invariant.
#[test]
fn base_envelope_never_grants_write_or_run() {
    let base = BaseExecutionEnvelope::for_workspace_root("/home/user/project", "test");

    for cap in &base.inner.capabilities {
        assert!(
            !matches!(cap, Capability::WritePath { .. }),
            "BaseExecutionEnvelope must not contain WritePath"
        );
        assert!(
            !matches!(cap, Capability::RunCommand { .. }),
            "BaseExecutionEnvelope must not contain RunCommand"
        );
    }

    // Also verify with session state and fetch host additions
    let base_with_extras = base.with_session_state().with_fetch_host("example.com");

    for cap in &base_with_extras.inner.capabilities {
        assert!(
            !matches!(cap, Capability::WritePath { .. }),
            "BaseExecutionEnvelope (with extras) must not contain WritePath"
        );
        assert!(
            !matches!(cap, Capability::RunCommand { .. }),
            "BaseExecutionEnvelope (with extras) must not contain RunCommand"
        );
    }

    // validate_no_write_or_command must pass
    assert!(base_with_extras.validate_no_write_or_command().is_ok());
}
