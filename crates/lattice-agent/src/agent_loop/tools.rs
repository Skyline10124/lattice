pub(crate) fn tool_output_is_ok(output: &str) -> bool {
    !output.starts_with("Error:") && !output.starts_with("Tool denied:")
}
