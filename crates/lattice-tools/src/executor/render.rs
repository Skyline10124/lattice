pub(super) fn render_file_slice(
    content: &str,
    offset: usize,
    limit: usize,
    line_numbers: bool,
) -> String {
    let total_lines = content.lines().count();
    let start_index = offset.saturating_sub(1);
    if start_index >= total_lines {
        return format!(
            "[No content: requested offset {} but file has {} lines]",
            offset, total_lines
        );
    }

    let mut rendered = Vec::new();
    let mut returned = 0usize;
    for (line_index, line) in content.lines().enumerate().skip(start_index) {
        if returned >= limit {
            break;
        }
        if line_numbers {
            rendered.push(format!("{:>6}\t{}", line_index + 1, line));
        } else {
            rendered.push(line.to_string());
        }
        returned += 1;
    }

    if start_index + returned < total_lines {
        rendered.push(format!(
            "[TRUNCATED: showing lines {}-{} of {}; call read_file with offset {} to continue]",
            offset,
            start_index + returned,
            total_lines,
            start_index + returned + 1
        ));
    }

    rendered.join("\n")
}

pub(super) fn render_todos(args: &serde_json::Value) -> Result<String, String> {
    let todos = args
        .get("todos")
        .and_then(|v| v.as_array())
        .ok_or_else(|| "todo_write requires a todos array".to_string())?;
    if todos.len() > 100 {
        return Err("todo_write accepts at most 100 todos".to_string());
    }

    let mut pending = 0usize;
    let mut in_progress = 0usize;
    let mut completed = 0usize;
    let mut rendered = Vec::new();

    for (index, todo) in todos.iter().enumerate() {
        let content = todo
            .get("content")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim();
        if content.is_empty() {
            return Err(format!("todo {} has empty content", index + 1));
        }
        let status = todo.get("status").and_then(|v| v.as_str()).unwrap_or("");
        match status {
            "pending" => pending += 1,
            "in_progress" => in_progress += 1,
            "completed" => completed += 1,
            _ => {
                return Err(format!(
                    "todo {} has invalid status '{}'; expected pending, in_progress, or completed",
                    index + 1,
                    status
                ));
            }
        }
        rendered.push(format!("{}. [{}] {}", index + 1, status, content));
    }

    if in_progress > 1 {
        return Err("todo_write allows at most one in_progress todo".to_string());
    }

    let mut lines = vec![format!(
        "Todo list updated: {} item(s), {} pending, {} in_progress, {} completed",
        todos.len(),
        pending,
        in_progress,
        completed
    )];
    lines.extend(rendered);
    Ok(lines.join("\n"))
}

pub(super) fn truncate_output(mut output: String, limit: usize, label: &str) -> String {
    if output.len() <= limit {
        return output;
    }
    output.truncate(limit);
    output.push_str(&format!(
        "\n[TRUNCATED: {} exceeded {} bytes]",
        label, limit
    ));
    output
}
