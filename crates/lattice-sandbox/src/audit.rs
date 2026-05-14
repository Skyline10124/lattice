/// Trait for audit event sinks. Abstracts AuditLog from lattice-agent.
pub trait AuditSink: Send + Sync {
    fn log_sync(&self, kind: AuditEventKind);
}

#[derive(Debug, Clone)]
pub enum AuditEventKind {
    ToolExec {
        tool: String,
        command: String,
        label: String,
        outcome: String,
    },
    HookDeny {
        hook: String,
        tool: String,
        command: String,
        reason: String,
        severity: String,
    },
}
