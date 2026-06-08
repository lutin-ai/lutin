// All []const u8 slices in these types are borrowed: the producer (a provider's
// complete()/stream() implementation) owns the backing memory and a consumer
// that wants to outlive the call must copy.

const std = @import("std");

pub const ReasoningEffort = enum { low, medium, high };

pub const ToolParameter = struct {
    name: []const u8,
    type_name: []const u8,
    description: []const u8,
    required: bool,
};

pub const ToolDefinition = struct {
    name: []const u8,
    description: []const u8,
    parameters: []const ToolParameter,
};

pub const ToolCall = struct {
    id: []const u8,
    name: []const u8,
    arguments_json: []const u8,
};

pub const ToolResult = struct {
    call_id: []const u8,
    content: []const u8,
    is_error: bool,
};

pub const Assistant = struct {
    text: []const u8,
    tool_calls: []const ToolCall,
    thinking: ?[]const u8,
};

pub const Message = union(enum) {
    system: []const u8,
    user: []const u8,
    assistant: Assistant,
    tool_result: ToolResult,
    summary: []const u8,
};

pub const Usage = struct {
    prompt_tokens: u32 = 0,
    completion_tokens: u32 = 0,
    total_tokens: u32 = 0,
};

pub const CompletionRequest = struct {
    model: []const u8,
    messages: []const Message,
    tools: []const ToolDefinition = &.{},
    temperature: ?f32 = null,
    presence_penalty: ?f32 = null,
    max_tokens: ?u32 = null,
};

pub const CompletionResponse = struct {
    text: []const u8,
    thinking: ?[]const u8,
    tool_calls: []const ToolCall,
    model: []const u8,
    usage: Usage,
};

pub const StreamEvent = union(enum) {
    reasoning: []const u8,
    delta: []const u8,
    tool_call_start: struct { id: []const u8, name: []const u8 },
    tool_call_delta: struct { id: []const u8, arguments: []const u8 },
    done: ?Usage,
};

pub const LlmError = error{ Http, Api, RateLimited, Json, Stream };

pub const ModelInfo = struct {
    id: []const u8,
    name: []const u8,
    context_length: ?u64,
};

test "Message.assistant constructs and switches" {
    const calls = [_]ToolCall{};
    const msg: Message = .{ .assistant = .{
        .text = "hi",
        .tool_calls = &calls,
        .thinking = null,
    } };
    switch (msg) {
        .assistant => |a| try std.testing.expectEqualStrings("hi", a.text),
        else => try std.testing.expect(false),
    }
}
