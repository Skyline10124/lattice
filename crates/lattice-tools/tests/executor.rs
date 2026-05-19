use lattice_llm::{FunctionCall, ToolCall};
use lattice_tools::{DefaultToolExecutor, ToolExecutor, ToolLimits};

fn tool_call(name: &str, args: serde_json::Value) -> ToolCall {
    ToolCall::new("call_test", FunctionCall::new(name, args.to_string()))
}

/// Trait extension for tests: flatten Result<String, String> into a single
/// String so legacy assertions on `.contains()` keep working. Failure values
/// are prefixed with "Error: " to preserve the assertion contract.
#[async_trait::async_trait]
trait ExecuteFlat {
    async fn execute_flat(&self, call: &ToolCall) -> String;
}

#[async_trait::async_trait]
impl ExecuteFlat for DefaultToolExecutor {
    async fn execute_flat(&self, call: &ToolCall) -> String {
        match self.execute(call).await {
            Ok(output) => output,
            Err(error) => format!("Error: {error}"),
        }
    }
}

fn executor_with_all_tools(root: &str) -> DefaultToolExecutor {
    DefaultToolExecutor::for_project_root(
        root,
        [
            "workspace_context",
            "todo_write",
            "read_file",
            "read_files",
            "write_file",
            "patch",
            "grep",
            "find_files",
            "list_directory",
            "bash",
            "web_search",
        ],
    )
    .unwrap()
}

#[tokio::test]
async fn read_file_supports_relative_paths_line_ranges_and_line_numbers() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("sample.txt"), "one\ntwo\nthree\nfour\n").unwrap();
    let executor = executor_with_all_tools(dir.path().to_str().unwrap());

    let result = executor
        .execute_flat(&tool_call(
            "read_file",
            serde_json::json!({
                "path": "sample.txt",
                "offset": 2,
                "limit": 2
            }),
        ))
        .await;

    assert!(result.contains("     2\ttwo"));
    assert!(result.contains("     3\tthree"));
    assert!(result.contains("[TRUNCATED: showing lines 2-3 of 4"));
    assert!(!result.contains("one"));
}

#[tokio::test]
async fn read_files_batches_targeted_file_reads() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.txt"), "a1\na2\na3\n").unwrap();
    std::fs::write(dir.path().join("b.txt"), "b1\nb2\n").unwrap();
    let executor = executor_with_all_tools(dir.path().to_str().unwrap());

    let result = executor
        .execute_flat(&tool_call(
            "read_files",
            serde_json::json!({
                "files": [
                    {"path": "a.txt", "offset": 2, "limit": 1},
                    {"path": "b.txt", "line_numbers": false}
                ]
            }),
        ))
        .await;

    assert!(result.contains("--- a.txt ---"));
    assert!(result.contains("     2\ta2"));
    assert!(!result.contains("     1\ta1"));
    assert!(result.contains("--- b.txt ---"));
    assert!(result.contains("b1\nb2"));
}

#[tokio::test]
async fn read_file_and_read_files_show_content_hash() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.txt"), "a1\na2\n").unwrap();
    std::fs::write(dir.path().join("b.txt"), "b1\n").unwrap();
    let executor = executor_with_all_tools(dir.path().to_str().unwrap());

    let single = executor
        .execute_flat(&tool_call(
            "read_file",
            serde_json::json!({
                "path": "a.txt"
            }),
        ))
        .await;

    let batch = executor
        .execute_flat(&tool_call(
            "read_files",
            serde_json::json!({
                "files": [
                    {"path": "a.txt"},
                    {"path": "b.txt"}
                ]
            }),
        ))
        .await;

    assert!(single.contains("Content hash: blake3:"));
    assert_eq!(batch.matches("Content hash: blake3:").count(), 2);
}

#[tokio::test]
async fn workspace_context_reports_entries_instructions_and_project_type() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("src")).unwrap();
    std::fs::write(dir.path().join("Cargo.toml"), "").unwrap();
    std::fs::write(dir.path().join("AGENTS.md"), "rules").unwrap();
    let executor = executor_with_all_tools(dir.path().to_str().unwrap());

    let result = executor
        .execute_flat(&tool_call(
            "workspace_context",
            serde_json::json!({
                "path": "."
            }),
        ))
        .await;

    assert!(result.contains("Workspace:"));
    assert!(result.contains("FILE Cargo.toml"));
    assert!(result.contains("DIR src"));
    assert!(result.contains("Project instruction files"));
    assert!(result.contains("AGENTS.md"));
    assert!(result.contains("Detected project files"));
    assert!(result.contains("cargo test"));
}

#[tokio::test]
async fn todo_write_validates_single_in_progress_item() {
    let dir = tempfile::tempdir().unwrap();
    let executor = executor_with_all_tools(dir.path().to_str().unwrap());

    let result = executor
        .execute_flat(&tool_call(
            "todo_write",
            serde_json::json!({
                "todos": [
                    {"content": "Inspect code", "status": "completed"},
                    {"content": "Apply patch", "status": "in_progress"},
                    {"content": "Run tests", "status": "pending"}
                ]
            }),
        ))
        .await;

    assert!(result.contains("Todo list updated: 3 item(s), 1 pending, 1 in_progress, 1 completed"));
    assert!(result.contains("2. [in_progress] Apply patch"));

    let invalid = executor
        .execute_flat(&tool_call(
            "todo_write",
            serde_json::json!({
                "todos": [
                    {"content": "One", "status": "in_progress"},
                    {"content": "Two", "status": "in_progress"}
                ]
            }),
        ))
        .await;

    assert!(invalid.contains("at most one in_progress"));
}

#[tokio::test]
async fn grep_caps_results_and_reports_truncation() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("sample.txt"), "hit 1\nhit 2\nhit 3\n").unwrap();
    let executor = executor_with_all_tools(dir.path().to_str().unwrap());

    let result = executor
        .execute_flat(&tool_call(
            "grep",
            serde_json::json!({
                "pattern": "hit",
                "path": ".",
                "max_results": 2
            }),
        ))
        .await;

    assert!(result.contains("sample.txt:1:hit 1"));
    assert!(result.contains("sample.txt:2:hit 2"));
    assert!(!result.contains("hit 3"));
    assert!(result.contains("[TRUNCATED: grep returned first 2 matches"));
}

#[tokio::test]
async fn list_directory_caps_entries_and_reports_total() {
    let dir = tempfile::tempdir().unwrap();
    for name in ["a.txt", "b.txt", "c.txt"] {
        std::fs::write(dir.path().join(name), name).unwrap();
    }
    let executor = executor_with_all_tools(dir.path().to_str().unwrap());

    let result = executor
        .execute_flat(&tool_call(
            "list_directory",
            serde_json::json!({
                "path": ".",
                "limit": 2
            }),
        ))
        .await;

    assert!(result.contains("FILE  a.txt"));
    assert!(result.contains("FILE  b.txt"));
    assert!(!result.contains("FILE  c.txt"));
    assert!(result.contains("[TRUNCATED: showing 2 of 3 entries"));
}

#[tokio::test]
async fn find_files_matches_names_and_relative_paths_with_limits() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("src/bin")).unwrap();
    std::fs::write(dir.path().join("src/lib.rs"), "").unwrap();
    std::fs::write(dir.path().join("src/bin/main.rs"), "").unwrap();
    std::fs::write(dir.path().join("README.md"), "").unwrap();
    let executor = executor_with_all_tools(dir.path().to_str().unwrap());

    let result = executor
        .execute_flat(&tool_call(
            "find_files",
            serde_json::json!({
                "pattern": "*.rs",
                "path": ".",
                "limit": 1
            }),
        ))
        .await;

    assert!(result.contains(".rs"));
    assert!(!result.contains("README.md"));
    assert!(result.contains("[TRUNCATED: find_files returned first 1 matches"));
}

#[tokio::test]
async fn patch_applies_structured_patch_and_returns_json_manifest() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("sample.txt");
    let before = "alpha\nbeta\ngamma\n";
    std::fs::write(&path, before).unwrap();
    let executor = executor_with_all_tools(dir.path().to_str().unwrap());
    let old_hash = lattice_tools::edit::content_hash(before.as_bytes());

    let result = executor
        .execute_flat(&tool_call(
            "patch",
            serde_json::json!({ "patch": format!(
                "\
*** Begin Patch
*** Update File: sample.txt
*** Old Hash: {old_hash}
@@
 alpha
-beta
+BETA
 gamma
*** End Patch"
            )}),
        ))
        .await;
    let value: serde_json::Value = serde_json::from_str(&result).unwrap();

    assert_eq!(value["ok"], true);
    assert_eq!(value["changed_files"][0]["path"], "sample.txt");
    assert_eq!(value["changed_files"][0]["operation"], "update");
    assert!(value["changed_files"][0]["bytes_before"].as_u64().unwrap() > 0);
    assert!(value["changed_files"][0]["bytes_after"].as_u64().unwrap() > 0);
    assert_eq!(
        std::fs::read_to_string(path).unwrap(),
        "alpha\nBETA\ngamma\n"
    );
}

#[tokio::test]
async fn patch_failure_json_keeps_changed_files_empty_and_attempts_populated() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("sample.txt");
    let before = "alpha\n";
    std::fs::write(&path, before).unwrap();
    let executor = executor_with_all_tools(dir.path().to_str().unwrap());
    let old_hash = lattice_tools::edit::content_hash(before.as_bytes());

    let result = executor
        .execute_flat(&tool_call(
            "patch",
            serde_json::json!({ "patch": format!(
                "\
*** Begin Patch
*** Update File: sample.txt
*** Old Hash: {old_hash}
@@
-missing
+new
*** End Patch"
            )}),
        ))
        .await;
    let value: serde_json::Value = serde_json::from_str(&result).unwrap();

    assert_eq!(value["ok"], false);
    assert_eq!(value["errors"][0]["code"], "hunk_missing");
    assert_eq!(value["changed_files"].as_array().unwrap().len(), 0);
    assert_eq!(value["attempted_operations"][0]["path"], "sample.txt");
    assert_eq!(std::fs::read_to_string(path).unwrap(), before);
}

#[tokio::test]
async fn authorized_executor_denies_unlisted_builtin_before_execution() {
    let dir = tempfile::tempdir().unwrap();
    let executor =
        DefaultToolExecutor::for_project_root(dir.path().to_str().unwrap(), ["read_file"]).unwrap();

    let result = executor
        .execute_flat(&tool_call(
            "write_file",
            serde_json::json!({
                "path": "denied.txt",
                "content": "should not write"
            }),
        ))
        .await;

    assert!(
        result.contains("Tool denied: 'write_file' is not enabled"),
        "denial should be plain enabled-tool text: {}",
        result
    );
    assert!(result.contains("read_file"));
    assert!(!dir.path().join("denied.txt").exists());
}

#[tokio::test]
async fn denied_tool_is_not_unknown_tool() {
    let dir = tempfile::tempdir().unwrap();
    let executor =
        DefaultToolExecutor::for_project_root(dir.path().to_str().unwrap(), ["read_file"]).unwrap();

    let result = executor
        .execute_flat(&tool_call(
            "bash",
            serde_json::json!({"command": "echo no"}),
        ))
        .await;

    assert!(
        result.contains("Tool denied: 'bash' is not enabled"),
        "denial should be plain enabled-tool text: {}",
        result
    );
    assert!(!result.contains("Unknown tool:"));
}

#[cfg(unix)]
#[tokio::test]
async fn bash_timeout_returns_promptly() {
    let dir = tempfile::tempdir().unwrap();

    let limits = ToolLimits {
        max_command_timeout: 1,
        ..ToolLimits::default()
    };

    let executor = DefaultToolExecutor::for_project_root(dir.path().to_str().unwrap(), ["bash"])
        .unwrap()
        .with_limits(limits);

    let started = std::time::Instant::now();

    let result = executor
        .execute_flat(&tool_call(
            "bash",
            serde_json::json!({"command": "exec sleep 5"}),
        ))
        .await;

    assert!(result.contains("timed out"), "got: {result}");
    assert!(
        started.elapsed() < std::time::Duration::from_secs(3),
        "timeout should return without waiting for the full command duration"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn bash_rejects_empty_command() {
    let dir = tempfile::tempdir().unwrap();
    let executor =
        DefaultToolExecutor::for_project_root(dir.path().to_str().unwrap(), ["bash"]).unwrap();

    let result = executor
        .execute_flat(&tool_call("bash", serde_json::json!({"command": "   "})))
        .await;

    assert!(result.contains("empty command"), "got: {result}");
}

#[cfg(unix)]
#[tokio::test]
async fn bash_allows_normal_shell_operators() {
    let dir = tempfile::tempdir().unwrap();
    let executor =
        DefaultToolExecutor::for_project_root(dir.path().to_str().unwrap(), ["bash"]).unwrap();

    let result = executor
        .execute_flat(&tool_call(
            "bash",
            serde_json::json!({"command": "echo hello > /dev/null && echo ok"}),
        ))
        .await;

    assert!(
        result.contains("ok"),
        "shell operators should be allowed, got: {result}"
    );
}

// ── project-root executor tests (v1.1 path containment) ──────────────

#[tokio::test]
async fn project_root_executor_rejects_parent_traversal() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("secret.txt"), "secret").unwrap();
    let sub = dir.path().join("sub");
    std::fs::create_dir(&sub).unwrap();
    std::fs::write(sub.join("ok.txt"), "ok").unwrap();
    let executor = DefaultToolExecutor::for_project_root(
        sub.to_str().unwrap(),
        [
            "read_file",
            "read_files",
            "write_file",
            "grep",
            "list_directory",
            "find_files",
        ],
    )
    .unwrap();

    // read_file with .. traversal
    let result = executor
        .execute_flat(&tool_call(
            "read_file",
            serde_json::json!({"path": "../secret.txt"}),
        ))
        .await;
    assert!(
        result.contains("traversal denied"),
        "should reject .. traversal, got: {}",
        result
    );

    // read_files with .. traversal
    let result = executor
        .execute_flat(&tool_call(
            "read_files",
            serde_json::json!({"files": [{"path": "../secret.txt"}]}),
        ))
        .await;
    assert!(
        result.contains("traversal denied"),
        "read_files: {}",
        result
    );

    // write_file with .. traversal
    let result = executor
        .execute_flat(&tool_call(
            "write_file",
            serde_json::json!({"path": "../evil.txt", "content": "bad"}),
        ))
        .await;
    assert!(
        result.contains("traversal denied"),
        "write_file: {}",
        result
    );

    // grep with .. traversal
    let result = executor
        .execute_flat(&tool_call(
            "grep",
            serde_json::json!({"pattern": "secret", "path": ".."}),
        ))
        .await;
    assert!(result.contains("traversal denied"), "grep: {}", result);

    // list_directory with .. traversal
    let result = executor
        .execute_flat(&tool_call(
            "list_directory",
            serde_json::json!({"path": ".."}),
        ))
        .await;
    assert!(
        result.contains("traversal denied"),
        "list_directory: {}",
        result
    );

    // find_files with .. traversal
    let result = executor
        .execute_flat(&tool_call(
            "find_files",
            serde_json::json!({"pattern": "*.txt", "path": ".."}),
        ))
        .await;
    assert!(
        result.contains("traversal denied"),
        "find_files: {}",
        result
    );
}

#[tokio::test]
async fn project_root_executor_rejects_symlink_escape() {
    let dir = tempfile::tempdir().unwrap();
    let project = dir.path().join("project");
    std::fs::create_dir(&project).unwrap();
    let outside = dir.path().join("outside");
    std::fs::create_dir(&outside).unwrap();
    std::fs::write(outside.join("secret.txt"), "secret data").unwrap();

    #[cfg(unix)]
    std::os::unix::fs::symlink(&outside, project.join("escape_link")).unwrap();

    let executor = DefaultToolExecutor::for_project_root(
        project.to_str().unwrap(),
        ["read_file", "grep", "list_directory", "find_files"],
    )
    .unwrap();

    // read_file through symlink escape
    let result = executor
        .execute_flat(&tool_call(
            "read_file",
            serde_json::json!({"path": "escape_link/secret.txt"}),
        ))
        .await;
    assert!(
        result.contains("outside the project root") || result.contains("IoError"),
        "should deny symlink escape for read_file, got: {}",
        result
    );

    // list_directory through symlink escape
    let result = executor
        .execute_flat(&tool_call(
            "list_directory",
            serde_json::json!({"path": "escape_link"}),
        ))
        .await;
    assert!(
        result.contains("outside the project root"),
        "should deny symlink escape for list_directory, got: {}",
        result
    );

    // grep through symlink escape
    let result = executor
        .execute_flat(&tool_call(
            "grep",
            serde_json::json!({"pattern": "secret", "path": "escape_link"}),
        ))
        .await;
    assert!(
        result.contains("(no matches)") || result.contains("outside"),
        "grep should not find matches outside root, got: {}",
        result
    );

    // find_files through symlink escape
    let result = executor
        .execute_flat(&tool_call(
            "find_files",
            serde_json::json!({"pattern": "*.txt", "path": "escape_link"}),
        ))
        .await;
    assert!(
        result.contains("(no matches)") || result.contains("outside"),
        "find_files should not find matches outside root, got: {}",
        result
    );
}

#[tokio::test]
async fn project_root_executor_allows_inside_project_root() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("hello.txt"), "hello world\n").unwrap();
    std::fs::create_dir(dir.path().join("src")).unwrap();
    std::fs::write(dir.path().join("src/main.rs"), "fn main() {}").unwrap();

    let executor = DefaultToolExecutor::for_project_root(
        dir.path().to_str().unwrap(),
        [
            "read_file",
            "write_file",
            "grep",
            "list_directory",
            "find_files",
            "patch",
        ],
    )
    .unwrap();

    // read_file inside root
    let result = executor
        .execute_flat(&tool_call(
            "read_file",
            serde_json::json!({"path": "hello.txt"}),
        ))
        .await;
    assert!(result.contains("hello world"), "read_file: {}", result);

    // grep inside root
    let result = executor
        .execute_flat(&tool_call(
            "grep",
            serde_json::json!({"pattern": "hello", "path": "."}),
        ))
        .await;
    assert!(result.contains("hello.txt"), "grep: {}", result);

    // list_directory inside root
    let result = executor
        .execute_flat(&tool_call(
            "list_directory",
            serde_json::json!({"path": "."}),
        ))
        .await;
    assert!(
        result.contains("FILE  hello.txt"),
        "list_directory: {}",
        result
    );
    assert!(result.contains("DIR  src"), "list_directory: {}", result);

    // find_files inside root
    let result = executor
        .execute_flat(&tool_call(
            "find_files",
            serde_json::json!({"pattern": "*.rs"}),
        ))
        .await;
    assert!(result.contains("main.rs"), "find_files: {}", result);

    // write_file inside root
    let result = executor
        .execute_flat(&tool_call(
            "write_file",
            serde_json::json!({"path": "output.txt", "content": "written"}),
        ))
        .await;
    assert!(result.contains("Wrote"), "write_file: {}", result);
    assert_eq!(
        std::fs::read_to_string(dir.path().join("output.txt")).unwrap(),
        "written"
    );
}

#[tokio::test]
async fn project_root_executor_keeps_output_limits() {
    let dir = tempfile::tempdir().unwrap();
    let content: String = (0..3000).map(|i| format!("line {i}\n")).collect();
    std::fs::write(dir.path().join("big.txt"), &content).unwrap();

    let executor = DefaultToolExecutor::for_project_root(
        dir.path().to_str().unwrap(),
        ["read_file", "grep", "list_directory", "find_files"],
    )
    .unwrap();

    // read_file respects DEFAULT_READ_LINE_LIMIT (2000)
    let result = executor
        .execute_flat(&tool_call(
            "read_file",
            serde_json::json!({"path": "big.txt"}),
        ))
        .await;
    assert!(
        result.contains("[TRUNCATED"),
        "read_file should truncate at line limit"
    );

    // grep respects DEFAULT_GREP_LIMIT (200)
    let result = executor
        .execute_flat(&tool_call(
            "grep",
            serde_json::json!({"pattern": "line", "path": "."}),
        ))
        .await;
    assert!(
        result.contains("[TRUNCATED"),
        "grep should truncate at result limit"
    );
}

#[tokio::test]
async fn project_root_executor_has_no_dynamic_approval() {
    let dir = tempfile::tempdir().unwrap();
    let executor =
        DefaultToolExecutor::for_project_root(dir.path().to_str().unwrap(), ["read_file"]).unwrap();

    let result = executor
        .execute_flat(&tool_call(
            "bash",
            serde_json::json!({"command": "echo test"}),
        ))
        .await;
    assert!(
        result.contains("Tool denied: 'bash' is not enabled"),
        "unlisted tool should be denied: {}",
        result
    );
}
