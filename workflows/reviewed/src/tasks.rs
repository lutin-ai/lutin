use std::path::{Path, PathBuf};

use async_trait::async_trait;
use lutin_llm::{ToolCall, ToolDefinition, ToolName, ToolParameter, ToolResultContent};
use lutin_tools::{Tool, ToolCallContext, ToolResult};
use serde::{Deserialize, Serialize};

const FILENAME: &str = "tasks.json";
const CREATE: &str = "create_task";
const START: &str = "start_task";
const FINISH: &str = "finish_task";
const DELETE: &str = "delete_task";
const DONE_SHOWN: usize = 5;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    Todo,
    InProgress,
    Done,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Task {
    pub id: u64,
    pub title: String,
    pub description: Option<String>,
    pub status: TaskStatus,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct TaskFile {
    next_id: u64,
    tasks: Vec<Task>,
}

fn path(state_dir: &Path) -> PathBuf {
    state_dir.join(FILENAME)
}

fn load(state_dir: &Path) -> TaskFile {
    match std::fs::read(path(state_dir)) {
        Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_default(),
        Err(_) => TaskFile::default(),
    }
}

fn save(state_dir: &Path, file: &TaskFile) -> Result<(), String> {
    std::fs::create_dir_all(state_dir).map_err(|e| format!("create state dir: {e}"))?;
    let body = serde_json::to_vec_pretty(file).map_err(|e| format!("serialise tasks: {e}"))?;
    let tmp = state_dir.join(format!("{FILENAME}.tmp"));
    std::fs::write(&tmp, &body).map_err(|e| format!("write tasks: {e}"))?;
    std::fs::rename(&tmp, path(state_dir)).map_err(|e| format!("rename tasks: {e}"))?;
    Ok(())
}

fn render(tasks: &[Task]) -> String {
    if tasks.is_empty() {
        return "No tasks.".into();
    }
    let mut out = String::new();
    for t in tasks.iter().filter(|t| t.status == TaskStatus::InProgress) {
        out.push_str(&format!("▸ #{} [in progress] {}", t.id, t.title));
        if let Some(d) = &t.description {
            out.push_str(&format!(" — {d}"));
        }
        out.push('\n');
    }
    for t in tasks.iter().filter(|t| t.status == TaskStatus::Todo) {
        out.push_str(&format!("  #{} [todo] {}", t.id, t.title));
        if let Some(d) = &t.description {
            out.push_str(&format!(" — {d}"));
        }
        out.push('\n');
    }
    let done: Vec<&Task> = tasks.iter().filter(|t| t.status == TaskStatus::Done).collect();
    let skipped = done.len().saturating_sub(DONE_SHOWN);
    if skipped > 0 {
        out.push_str(&format!("  …{skipped} older finished task(s)\n"));
    }
    for t in done.iter().rev().take(DONE_SHOWN).rev() {
        out.push_str(&format!("✓ #{} {}\n", t.id, t.title));
    }
    out
}

pub fn prompt_block(state_dir: &Path) -> String {
    let file = load(state_dir);
    format!(
        "\n\n## Tasks\n\nYour task list. Use create_task to plan upcoming work (a title is \
         enough), start_task before acting on one (a description of the approach is required by \
         then, and only one task may be in progress at a time), finish_task when it is verified \
         done, and delete_task for work that is no longer needed.\n\n{}",
        render(&file.tasks)
    )
}

pub fn reviewer_block(state_dir: &Path) -> Option<String> {
    let file = load(state_dir);
    if file.tasks.is_empty() {
        return None;
    }
    Some(render(&file.tasks))
}

pub fn tools(state_dir: PathBuf) -> Vec<Box<dyn Tool>> {
    vec![
        Box::new(CreateTask { state_dir: state_dir.clone() }),
        Box::new(StartTask { state_dir: state_dir.clone() }),
        Box::new(FinishTask { state_dir: state_dir.clone() }),
        Box::new(DeleteTask { state_dir }),
    ]
}

struct CreateTask {
    state_dir: PathBuf,
}
struct StartTask {
    state_dir: PathBuf,
}
struct FinishTask {
    state_dir: PathBuf,
}
struct DeleteTask {
    state_dir: PathBuf,
}

#[async_trait]
impl Tool for CreateTask {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: ToolName::new(CREATE),
            description: "Add a task to the task list. A title alone is fine when planning \
                ahead; the description can be supplied later when the task is started."
                .into(),
            parameters: vec![
                ToolParameter {
                    name: "title".into(),
                    r#type: "string".into(),
                    description: "Short imperative title, e.g. \"Wire codec for task events\"."
                        .into(),
                    required: true,
                },
                ToolParameter {
                    name: "description".into(),
                    r#type: "string".into(),
                    description: "What the task involves and how it will be verified. Optional \
                        at creation; required by the time the task is started."
                        .into(),
                    required: false,
                },
            ],
        }
    }

    async fn call(&self, _ctx: &ToolCallContext, call: ToolCall) -> ToolResult {
        let Some(title) = arg_str(&call, "title") else {
            return err(call.id, "create_task: missing 'title'".into());
        };
        let description = arg_str(&call, "description");
        let mut file = load(&self.state_dir);
        file.next_id += 1;
        let id = file.next_id;
        file.tasks.push(Task {
            id,
            title,
            description,
            status: TaskStatus::Todo,
        });
        persist(call.id, &self.state_dir, file, format!("Created task #{id}."))
    }
}

#[async_trait]
impl Tool for StartTask {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: ToolName::new(START),
            description: "Mark a todo task as in progress. Only one task may be in progress at \
                a time. The task must have a description by this point — pass one here if it was \
                created with only a title."
                .into(),
            parameters: vec![
                ToolParameter {
                    name: "id".into(),
                    r#type: "integer".into(),
                    description: "Task id.".into(),
                    required: true,
                },
                ToolParameter {
                    name: "description".into(),
                    r#type: "string".into(),
                    description: "What the task involves and how it will be verified. Required \
                        if the task has no description yet; otherwise replaces it."
                        .into(),
                    required: false,
                },
            ],
        }
    }

    async fn call(&self, _ctx: &ToolCallContext, call: ToolCall) -> ToolResult {
        let Some(id) = arg_id(&call) else {
            return err(call.id, "start_task: missing integer 'id'".into());
        };
        let description = arg_str(&call, "description");
        let mut file = load(&self.state_dir);
        if let Some(active) = file.tasks.iter().find(|t| t.status == TaskStatus::InProgress) {
            return err(
                call.id,
                format!(
                    "start_task: task #{} ({}) is already in progress — finish or delete it first",
                    active.id, active.title
                ),
            );
        }
        let Some(task) = file.tasks.iter_mut().find(|t| t.id == id) else {
            return err(call.id, format!("start_task: no task #{id}"));
        };
        if task.status != TaskStatus::Todo {
            return err(call.id, format!("start_task: task #{id} is not a todo"));
        }
        if description.is_some() {
            task.description = description;
        }
        if task.description.is_none() {
            return err(
                call.id,
                format!(
                    "start_task: task #{id} has no description — pass one describing the \
                     approach and how the work will be verified"
                ),
            );
        }
        task.status = TaskStatus::InProgress;
        let title = task.title.clone();
        persist(call.id, &self.state_dir, file, format!("Started task #{id}: {title}."))
    }
}

#[async_trait]
impl Tool for FinishTask {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: ToolName::new(FINISH),
            description: "Mark the in-progress task as done. Only call once the work is \
                actually complete and verified."
                .into(),
            parameters: vec![ToolParameter {
                name: "id".into(),
                r#type: "integer".into(),
                description: "Task id.".into(),
                required: true,
            }],
        }
    }

    async fn call(&self, _ctx: &ToolCallContext, call: ToolCall) -> ToolResult {
        let Some(id) = arg_id(&call) else {
            return err(call.id, "finish_task: missing integer 'id'".into());
        };
        let mut file = load(&self.state_dir);
        let Some(task) = file.tasks.iter_mut().find(|t| t.id == id) else {
            return err(call.id, format!("finish_task: no task #{id}"));
        };
        if task.status != TaskStatus::InProgress {
            return err(
                call.id,
                format!("finish_task: task #{id} is not in progress — start it first"),
            );
        }
        task.status = TaskStatus::Done;
        let title = task.title.clone();
        persist(call.id, &self.state_dir, file, format!("Finished task #{id}: {title}."))
    }
}

#[async_trait]
impl Tool for DeleteTask {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: ToolName::new(DELETE),
            description: "Remove a task that is no longer needed.".into(),
            parameters: vec![ToolParameter {
                name: "id".into(),
                r#type: "integer".into(),
                description: "Task id.".into(),
                required: true,
            }],
        }
    }

    async fn call(&self, _ctx: &ToolCallContext, call: ToolCall) -> ToolResult {
        let Some(id) = arg_id(&call) else {
            return err(call.id, "delete_task: missing integer 'id'".into());
        };
        let mut file = load(&self.state_dir);
        let before = file.tasks.len();
        file.tasks.retain(|t| t.id != id);
        if file.tasks.len() == before {
            return err(call.id, format!("delete_task: no task #{id}"));
        }
        persist(call.id, &self.state_dir, file, format!("Deleted task #{id}."))
    }
}

fn persist(
    call_id: lutin_llm::CallId,
    state_dir: &Path,
    file: TaskFile,
    headline: String,
) -> ToolResult {
    if let Err(e) = save(state_dir, &file) {
        return err(call_id, format!("task store: {e}"));
    }
    ok(call_id, format!("{headline}\n\nTasks:\n{}", render(&file.tasks)))
}

fn arg_str(call: &ToolCall, key: &str) -> Option<String> {
    call.arguments
        .get(key)
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

fn arg_id(call: &ToolCall) -> Option<u64> {
    call.arguments.get("id").and_then(|v| v.as_u64())
}

fn ok(call_id: lutin_llm::CallId, content: String) -> ToolResult {
    ToolResult::Ok(ToolResultContent {
        call_id,
        content,
        is_error: false,
    })
}

fn err(call_id: lutin_llm::CallId, content: String) -> ToolResult {
    ToolResult::Ok(ToolResultContent {
        call_id,
        content,
        is_error: true,
    })
}
