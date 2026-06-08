const std = @import("std");
const String = @import("../utility/string.zig").String;
const BoundedString = @import("../utility/string.zig").BoundedString;
const Datetime = @import("../utility/datetime.zig").Datetime;
const Uuid = @import("../utility/uuid.zig").Uuid;

pub const Persona = struct {
    id: Uuid,
    name: String,

    display_name: String,
    description: String,

    system_prompt: String,

    category: String,
};

pub const LlmProvider = struct {
    name: String,
    // TODO: API url, type etc?
};

pub const LlmModel = struct {
    display_name: String,
    provider_name: String,
    model_name: String,
    temperature: ?f32,
    presence_penalty: ?f32,
    context_limit: ?u32,
    sliding_window_length: ?u32,

    thinking: Thinking,

    pub const Thinking = enum {
        disabled,
        low,
        medium,
        high,
        xhigh,
    };
};

pub const ProjectEntry = struct {
    session_id: Uuid,
    title: String,
    summary: String,

    created_at: Datetime,
    updated_at: Datetime,
};

pub const Session = struct {
    id: Uuid,
    messages: std.ArrayList(Message),

    persona_id: Uuid,

    pub const Content = union(enum) {};
};

pub const Project = struct {
    id: Uuid,
    title: String,

    session_summaries: std.ArrayList(Session.Summary),

    created_at: Datetime,
    updated_at: Datetime,
};

pub const Message = struct {
    content: Content,

    created_at: Datetime,
    updated_at: Datetime,

    /// Using model at the time of prompting
    output_tokens: u32,

    const Self = @This();

    pub const Content = union(enum) {
        user: String,
        assistant: String,
        thinking: String,

        image: ImageContent,
        tool: ToolContent,
    };
};

pub const ImageContent = struct {
    media_type: BoundedString(32),
    base64: String,
};

pub const ToolContent = struct {
    tool_name: BoundedString(64),
    call_id: String,
    input: String,
    output: String,
    is_error: bool,
};
