const std = @import("std");
const types = @import("../llm/types.zig");
const approval = @import("approval.zig");

pub const ExecResult = struct {
    output: []u8,
    is_error: bool,
};

pub const ToolExec = struct {
    pub const Tool = struct {
        def: types.ToolDefinition,
        run: *const fn (ctx: *anyopaque, alloc: std.mem.Allocator, args_json: []const u8) anyerror!ExecResult,
        ctx: *anyopaque,
    };
};

pub const ApprovalRequest = struct {
    persona: []const u8,
    tool: []const u8,
    args_preview: []const u8,
};

pub const ApprovalAnswer = enum { allow_once, allow_persist, deny_once, deny_persist };

pub const Hooks = struct {
    ctx: *anyopaque,
    ask_approval: *const fn (ctx: *anyopaque, req: ApprovalRequest) anyerror!ApprovalAnswer,
    on_event: *const fn (ctx: *anyopaque, ev: types.StreamEvent) anyerror!void,
    on_turn: *const fn (ctx: *anyopaque, msg: types.Message) anyerror!void,
};

pub const Settings = struct {
    model: []const u8,
    persona: []const u8,
    temperature: ?f32 = null,
    max_tokens: ?u32 = null,
    max_turns: u32 = 24,
};

pub fn freeMessage(alloc: std.mem.Allocator, msg: *types.Message) void {
    switch (msg.*) {
        .system => |s| alloc.free(s),
        .user => |s| alloc.free(s),
        .summary => |s| alloc.free(s),
        .assistant => |a| {
            alloc.free(a.text);
            if (a.thinking) |t| alloc.free(t);
            for (a.tool_calls) |tc| {
                alloc.free(tc.id);
                alloc.free(tc.name);
                alloc.free(tc.arguments_json);
            }
            alloc.free(a.tool_calls);
        },
        .tool_result => |tr| {
            alloc.free(tr.call_id);
            alloc.free(tr.content);
        },
    }
}

const ToolAcc = struct {
    id: []u8,
    name: []u8,
    args: std.ArrayList(u8),
};

const Capture = struct {
    alloc: std.mem.Allocator,
    text: std.ArrayList(u8),
    thinking: std.ArrayList(u8),
    has_thinking: bool,
    calls: std.ArrayList(ToolAcc),
    hooks: Hooks,
    forward_err: ?anyerror,

    fn init(alloc: std.mem.Allocator, hooks: Hooks) Capture {
        return .{
            .alloc = alloc,
            .text = .empty,
            .thinking = .empty,
            .has_thinking = false,
            .calls = .empty,
            .hooks = hooks,
            .forward_err = null,
        };
    }

    fn deinit(self: *Capture) void {
        self.text.deinit(self.alloc);
        self.thinking.deinit(self.alloc);
        for (self.calls.items) |*c| {
            self.alloc.free(c.id);
            self.alloc.free(c.name);
            c.args.deinit(self.alloc);
        }
        self.calls.deinit(self.alloc);
    }

    fn findCall(self: *Capture, id: []const u8) ?*ToolAcc {
        for (self.calls.items) |*c| {
            if (std.mem.eql(u8, c.id, id)) return c;
        }
        return null;
    }
};

fn captureEvent(ctx: *anyopaque, ev: types.StreamEvent) anyerror!void {
    const cap: *Capture = @ptrCast(@alignCast(ctx));

    cap.hooks.on_event(cap.hooks.ctx, ev) catch |e| {
        cap.forward_err = e;
    };

    switch (ev) {
        .delta => |s| try cap.text.appendSlice(cap.alloc, s),
        .reasoning => |s| {
            cap.has_thinking = true;
            try cap.thinking.appendSlice(cap.alloc, s);
        },
        .tool_call_start => |t| {
            if (cap.findCall(t.id) == null) {
                const acc: ToolAcc = .{
                    .id = try cap.alloc.dupe(u8, t.id),
                    .name = try cap.alloc.dupe(u8, t.name),
                    .args = .empty,
                };
                try cap.calls.append(cap.alloc, acc);
            }
        },
        .tool_call_delta => |t| {
            if (cap.findCall(t.id)) |c| {
                try c.args.appendSlice(cap.alloc, t.arguments);
            } else {
                const acc: ToolAcc = .{
                    .id = try cap.alloc.dupe(u8, t.id),
                    .name = try cap.alloc.dupe(u8, ""),
                    .args = .empty,
                };
                try cap.calls.append(cap.alloc, acc);
                var last = &cap.calls.items[cap.calls.items.len - 1];
                try last.args.appendSlice(cap.alloc, t.arguments);
            }
        },
        .done => {},
    }
}

fn buildAssistant(alloc: std.mem.Allocator, cap: *Capture) !types.Assistant {
    const text = try alloc.dupe(u8, cap.text.items);
    errdefer alloc.free(text);

    var thinking: ?[]const u8 = null;
    if (cap.has_thinking) {
        thinking = try alloc.dupe(u8, cap.thinking.items);
    }
    errdefer if (thinking) |t| alloc.free(t);

    const tool_calls = try alloc.alloc(types.ToolCall, cap.calls.items.len);
    var filled: usize = 0;
    errdefer {
        for (tool_calls[0..filled]) |tc| {
            alloc.free(tc.id);
            alloc.free(tc.name);
            alloc.free(tc.arguments_json);
        }
        alloc.free(tool_calls);
    }

    for (cap.calls.items, 0..) |c, i| {
        const id = try alloc.dupe(u8, c.id);
        errdefer alloc.free(id);
        const name = try alloc.dupe(u8, c.name);
        errdefer alloc.free(name);
        const args = try alloc.dupe(u8, c.args.items);
        tool_calls[i] = .{ .id = id, .name = name, .arguments_json = args };
        filled = i + 1;
    }

    return .{ .text = text, .tool_calls = tool_calls, .thinking = thinking };
}

fn lookupTool(tools: []const ToolExec.Tool, name: []const u8) ?*const ToolExec.Tool {
    for (tools) |*t| {
        if (std.mem.eql(u8, t.def.name, name)) return t;
    }
    return null;
}

fn pushToolResult(
    alloc: std.mem.Allocator,
    transcript: *std.ArrayList(types.Message),
    call_id: []const u8,
    content: []const u8,
    is_error: bool,
) !void {
    const id_dup = try alloc.dupe(u8, call_id);
    errdefer alloc.free(id_dup);
    const content_dup = try alloc.dupe(u8, content);
    try transcript.append(alloc, .{ .tool_result = .{
        .call_id = id_dup,
        .content = content_dup,
        .is_error = is_error,
    } });
}

fn pushToolResultOwned(
    alloc: std.mem.Allocator,
    transcript: *std.ArrayList(types.Message),
    call_id: []const u8,
    content_owned: []u8,
    is_error: bool,
) !void {
    const id_dup = try alloc.dupe(u8, call_id);
    errdefer alloc.free(id_dup);
    try transcript.append(alloc, .{ .tool_result = .{
        .call_id = id_dup,
        .content = content_owned,
        .is_error = is_error,
    } });
}

pub fn run(
    alloc: std.mem.Allocator,
    transcript: *std.ArrayList(types.Message),
    tools: []const ToolExec.Tool,
    provider: anytype,
    policy: *approval.Policy,
    hooks: Hooks,
    settings: Settings,
) !void {
    const tool_defs = try alloc.alloc(types.ToolDefinition, tools.len);
    defer alloc.free(tool_defs);
    for (tools, 0..) |t, i| tool_defs[i] = t.def;

    var turn: u32 = 0;
    while (true) : (turn += 1) {
        const req: types.CompletionRequest = .{
            .model = settings.model,
            .messages = transcript.items,
            .tools = tool_defs,
            .temperature = settings.temperature,
            .max_tokens = settings.max_tokens,
        };

        var cap = Capture.init(alloc, hooks);
        defer cap.deinit();

        try provider.stream(req, @as(*anyopaque, @ptrCast(&cap)), captureEvent);
        if (cap.forward_err) |e| return e;

        const assistant = try buildAssistant(alloc, &cap);
        try transcript.append(alloc, .{ .assistant = assistant });
        try hooks.on_turn(hooks.ctx, transcript.items[transcript.items.len - 1]);

        if (assistant.tool_calls.len == 0) return;

        for (assistant.tool_calls) |call| {
            const maybe_tool = lookupTool(tools, call.name);
            if (maybe_tool == null) {
                var buf: std.ArrayList(u8) = .empty;
                defer buf.deinit(alloc);
                try buf.appendSlice(alloc, "unknown tool: ");
                try buf.appendSlice(alloc, call.name);
                try pushToolResult(alloc, transcript, call.id, buf.items, true);
                continue;
            }
            const tool = maybe_tool.?;

            var decision = policy.decide(settings.persona, call.name);
            if (decision == .ask) {
                const ans = hooks.ask_approval(hooks.ctx, .{
                    .persona = settings.persona,
                    .tool = call.name,
                    .args_preview = call.arguments_json,
                }) catch ApprovalAnswer.deny_once;
                switch (ans) {
                    .allow_once => decision = .allow,
                    .allow_persist => {
                        try policy.remember(settings.persona, call.name, .allow);
                        decision = .allow;
                    },
                    .deny_once => decision = .deny,
                    .deny_persist => {
                        try policy.remember(settings.persona, call.name, .deny);
                        decision = .deny;
                    },
                }
            }

            if (decision == .deny) {
                try pushToolResult(alloc, transcript, call.id, "tool denied by policy", true);
                continue;
            }

            const exec = tool.run(tool.ctx, alloc, call.arguments_json) catch |err| {
                var buf: std.ArrayList(u8) = .empty;
                defer buf.deinit(alloc);
                try buf.appendSlice(alloc, "tool failed: ");
                try buf.appendSlice(alloc, @errorName(err));
                try pushToolResult(alloc, transcript, call.id, buf.items, true);
                continue;
            };
            try pushToolResultOwned(alloc, transcript, call.id, exec.output, exec.is_error);
        }

        if (turn + 1 >= settings.max_turns) {
            const note = try alloc.dupe(u8, "agent loop hit max_turns");
            try transcript.append(alloc, .{ .system = note });
            return;
        }
    }
}

// ---------------- tests ----------------

const testing = std.testing;

const Canned = struct {
    events: []const types.StreamEvent,
};

const MockProvider = struct {
    turns: []const Canned,
    idx: usize = 0,

    pub fn stream(
        self: *MockProvider,
        req: types.CompletionRequest,
        ctx: *anyopaque,
        onEvent: *const fn (ctx: *anyopaque, ev: types.StreamEvent) anyerror!void,
    ) !void {
        _ = req;
        if (self.idx >= self.turns.len) return error.Stream;
        const t = self.turns[self.idx];
        self.idx += 1;
        for (t.events) |e| try onEvent(ctx, e);
    }

    pub fn complete(self: *MockProvider, req: types.CompletionRequest) !types.CompletionResponse {
        _ = self;
        _ = req;
        return error.Stream;
    }
};

const TestHookCtx = struct {
    answer: ApprovalAnswer = .allow_once,
    events_seen: usize = 0,
    turns_seen: usize = 0,
};

fn testAsk(ctx: *anyopaque, req: ApprovalRequest) anyerror!ApprovalAnswer {
    _ = req;
    const c: *TestHookCtx = @ptrCast(@alignCast(ctx));
    return c.answer;
}

fn testOnEvent(ctx: *anyopaque, ev: types.StreamEvent) anyerror!void {
    _ = ev;
    const c: *TestHookCtx = @ptrCast(@alignCast(ctx));
    c.events_seen += 1;
}

fn testOnTurn(ctx: *anyopaque, msg: types.Message) anyerror!void {
    _ = msg;
    const c: *TestHookCtx = @ptrCast(@alignCast(ctx));
    c.turns_seen += 1;
}

const EchoCtx = struct {
    invocations: usize = 0,
};

fn echoRun(ctx: *anyopaque, alloc: std.mem.Allocator, args_json: []const u8) anyerror!ExecResult {
    const c: *EchoCtx = @ptrCast(@alignCast(ctx));
    c.invocations += 1;
    const out = try alloc.dupe(u8, args_json);
    return .{ .output = out, .is_error = false };
}

fn freeTranscript(alloc: std.mem.Allocator, transcript: *std.ArrayList(types.Message)) void {
    for (transcript.items) |*m| freeMessage(alloc, m);
    transcript.deinit(alloc);
}

test "loop runs a tool then exits when assistant produces no more calls" {
    const alloc = testing.allocator;

    const turn1_events = [_]types.StreamEvent{
        .{ .delta = "calling tool" },
        .{ .tool_call_start = .{ .id = "c1", .name = "echo" } },
        .{ .tool_call_delta = .{ .id = "c1", .arguments = "{\"x\":1}" } },
        .{ .done = null },
    };
    const turn2_events = [_]types.StreamEvent{
        .{ .delta = "all done" },
        .{ .done = null },
    };
    const turns = [_]Canned{
        .{ .events = &turn1_events },
        .{ .events = &turn2_events },
    };
    var mock = MockProvider{ .turns = &turns };

    var echo_ctx = EchoCtx{};
    const tools = [_]ToolExec.Tool{.{
        .def = .{ .name = "echo", .description = "echo", .parameters = &.{} },
        .run = echoRun,
        .ctx = @ptrCast(&echo_ctx),
    }};

    var policy = approval.Policy.empty(alloc);
    defer policy.deinit();
    try policy.remember("p", "echo", .allow);

    var hctx = TestHookCtx{};
    const hooks: Hooks = .{
        .ctx = @ptrCast(&hctx),
        .ask_approval = testAsk,
        .on_event = testOnEvent,
        .on_turn = testOnTurn,
    };

    var transcript: std.ArrayList(types.Message) = .empty;
    defer freeTranscript(alloc, &transcript);
    try transcript.append(alloc, .{ .user = try alloc.dupe(u8, "hi") });

    try run(alloc, &transcript, &tools, &mock, &policy, hooks, .{
        .model = "m",
        .persona = "p",
    });

    try testing.expectEqual(@as(usize, 4), transcript.items.len);
    try testing.expect(transcript.items[0] == .user);
    try testing.expect(transcript.items[1] == .assistant);
    try testing.expectEqual(@as(usize, 1), transcript.items[1].assistant.tool_calls.len);
    try testing.expect(transcript.items[2] == .tool_result);
    try testing.expectEqualStrings("{\"x\":1}", transcript.items[2].tool_result.content);
    try testing.expect(transcript.items[3] == .assistant);
    try testing.expectEqual(@as(usize, 0), transcript.items[3].assistant.tool_calls.len);
    try testing.expectEqual(@as(usize, 1), echo_ctx.invocations);
    try testing.expectEqual(@as(usize, 2), hctx.turns_seen);
}

test "policy deny short-circuits without invoking the tool" {
    const alloc = testing.allocator;

    const turn1_events = [_]types.StreamEvent{
        .{ .tool_call_start = .{ .id = "c1", .name = "echo" } },
        .{ .tool_call_delta = .{ .id = "c1", .arguments = "{}" } },
        .{ .done = null },
    };
    const turn2_events = [_]types.StreamEvent{ .{ .delta = "ok" }, .{ .done = null } };
    const turns = [_]Canned{
        .{ .events = &turn1_events },
        .{ .events = &turn2_events },
    };
    var mock = MockProvider{ .turns = &turns };

    var echo_ctx = EchoCtx{};
    const tools = [_]ToolExec.Tool{.{
        .def = .{ .name = "echo", .description = "echo", .parameters = &.{} },
        .run = echoRun,
        .ctx = @ptrCast(&echo_ctx),
    }};

    var policy = approval.Policy.empty(alloc);
    defer policy.deinit();
    try policy.remember("p", "echo", .deny);

    var hctx = TestHookCtx{};
    const hooks: Hooks = .{
        .ctx = @ptrCast(&hctx),
        .ask_approval = testAsk,
        .on_event = testOnEvent,
        .on_turn = testOnTurn,
    };

    var transcript: std.ArrayList(types.Message) = .empty;
    defer freeTranscript(alloc, &transcript);
    try transcript.append(alloc, .{ .user = try alloc.dupe(u8, "hi") });

    try run(alloc, &transcript, &tools, &mock, &policy, hooks, .{
        .model = "m",
        .persona = "p",
    });

    try testing.expectEqual(@as(usize, 0), echo_ctx.invocations);
    try testing.expect(transcript.items[2] == .tool_result);
    try testing.expect(transcript.items[2].tool_result.is_error);
    try testing.expectEqualStrings("tool denied by policy", transcript.items[2].tool_result.content);
}

test "max_turns=1 stops after the first turn" {
    const alloc = testing.allocator;

    const turn1_events = [_]types.StreamEvent{
        .{ .tool_call_start = .{ .id = "c1", .name = "echo" } },
        .{ .tool_call_delta = .{ .id = "c1", .arguments = "{}" } },
        .{ .done = null },
    };
    const turn2_events = [_]types.StreamEvent{
        .{ .tool_call_start = .{ .id = "c2", .name = "echo" } },
        .{ .tool_call_delta = .{ .id = "c2", .arguments = "{}" } },
        .{ .done = null },
    };
    const turns = [_]Canned{
        .{ .events = &turn1_events },
        .{ .events = &turn2_events },
    };
    var mock = MockProvider{ .turns = &turns };

    var echo_ctx = EchoCtx{};
    const tools = [_]ToolExec.Tool{.{
        .def = .{ .name = "echo", .description = "echo", .parameters = &.{} },
        .run = echoRun,
        .ctx = @ptrCast(&echo_ctx),
    }};

    var policy = approval.Policy.empty(alloc);
    defer policy.deinit();
    try policy.remember("p", "echo", .allow);

    var hctx = TestHookCtx{};
    const hooks: Hooks = .{
        .ctx = @ptrCast(&hctx),
        .ask_approval = testAsk,
        .on_event = testOnEvent,
        .on_turn = testOnTurn,
    };

    var transcript: std.ArrayList(types.Message) = .empty;
    defer freeTranscript(alloc, &transcript);
    try transcript.append(alloc, .{ .user = try alloc.dupe(u8, "hi") });

    try run(alloc, &transcript, &tools, &mock, &policy, hooks, .{
        .model = "m",
        .persona = "p",
        .max_turns = 1,
    });

    try testing.expectEqual(@as(usize, 1), mock.idx);
    try testing.expectEqual(@as(usize, 1), echo_ctx.invocations);
    try testing.expect(transcript.items[transcript.items.len - 1] == .system);
}
