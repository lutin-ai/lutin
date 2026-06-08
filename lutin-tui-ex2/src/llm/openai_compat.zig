const std = @import("std");
const types = @import("types.zig");
const http = @import("../net/http.zig");

const Allocator = std.mem.Allocator;
const Io = std.Io;

const Message = types.Message;
const ToolDefinition = types.ToolDefinition;
const ToolCall = types.ToolCall;
const CompletionRequest = types.CompletionRequest;
const CompletionResponse = types.CompletionResponse;
const StreamEvent = types.StreamEvent;
const Usage = types.Usage;
const LlmError = types.LlmError;
const ModelInfo = types.ModelInfo;

pub const Provider = struct {
    alloc: Allocator,
    io: Io,
    base_url: []const u8,
    api_key: []const u8,
    extra_headers: []const http.Header = &.{},
    last_error: ?[]u8 = null,

    pub fn deinit(self: *Provider) void {
        if (self.last_error) |s| self.alloc.free(s);
        self.last_error = null;
    }

    fn setLastError(self: *Provider, detail: ?[]u8) void {
        if (self.last_error) |s| self.alloc.free(s);
        self.last_error = detail;
    }

    pub fn complete(self: *Provider, req: CompletionRequest) LlmError!CompletionResponse {
        const body = buildRequestJson(self.alloc, req, false) catch return error.Json;
        defer self.alloc.free(body);

        const url = std.mem.concat(self.alloc, u8, &.{ trimTrailingSlash(self.base_url), "/chat/completions" }) catch return error.Json;
        defer self.alloc.free(url);

        var auth = buildAuthHeaders(self.alloc, self.api_key, self.extra_headers) catch return error.Json;
        defer auth.deinit(self.alloc);

        var detail: ?[]u8 = null;
        const resp = http.postJsonDetail(self.alloc, self.io, url, auth.headers, body, &detail) catch |e| {
            self.setLastError(detail);
            return mapHttp(e);
        };
        defer self.alloc.free(resp.body);

        return parseCompletion(self.alloc, resp.body);
    }

    pub fn stream(
        self: *Provider,
        req: CompletionRequest,
        ctx: anytype,
        comptime onEvent: fn (@TypeOf(ctx), StreamEvent) anyerror!void,
    ) LlmError!void {
        const body = buildRequestJson(self.alloc, req, true) catch return error.Json;
        defer self.alloc.free(body);

        const url = std.mem.concat(self.alloc, u8, &.{ trimTrailingSlash(self.base_url), "/chat/completions" }) catch return error.Json;
        defer self.alloc.free(url);

        var auth = buildAuthHeaders(self.alloc, self.api_key, self.extra_headers) catch return error.Json;
        defer auth.deinit(self.alloc);

        var st = StreamState(@TypeOf(ctx), onEvent).init(self.alloc, ctx);
        defer st.deinit();

        var detail: ?[]u8 = null;
        http.streamSseDetail(self.alloc, self.io, url, auth.headers, body, &st, StreamState(@TypeOf(ctx), onEvent).onSse, &detail) catch |e| {
            self.setLastError(detail);
            return mapHttp(e);
        };

        st.finish() catch return error.Stream;
    }

    pub fn models(self: *Provider) LlmError![]ModelInfo {
        const url = std.mem.concat(self.alloc, u8, &.{ trimTrailingSlash(self.base_url), "/models" }) catch return error.Json;
        defer self.alloc.free(url);

        var auth = buildAuthHeaders(self.alloc, self.api_key, self.extra_headers) catch return error.Json;
        defer auth.deinit(self.alloc);

        const resp = http.getJson(self.alloc, self.io, url, auth.headers) catch |e| return mapHttp(e);
        defer self.alloc.free(resp.body);

        return parseModels(self.alloc, resp.body);
    }
};

const AuthHeaders = struct {
    headers: []http.Header,
    bearer: ?[]u8,

    fn deinit(self: *AuthHeaders, alloc: Allocator) void {
        if (self.bearer) |b| alloc.free(b);
        alloc.free(self.headers);
    }
};

fn mapHttp(e: http.HttpError) LlmError {
    return switch (e) {
        error.BadStatus => error.Api,
        error.OutOfMemory => error.Json,
        else => error.Http,
    };
}

fn trimTrailingSlash(s: []const u8) []const u8 {
    var end = s.len;
    while (end > 0 and s[end - 1] == '/') end -= 1;
    return s[0..end];
}

fn buildAuthHeaders(alloc: Allocator, api_key: []const u8, extra: []const http.Header) !AuthHeaders {
    var list = std.array_list.Managed(http.Header).init(alloc);
    errdefer list.deinit();
    var bearer: ?[]u8 = null;
    errdefer if (bearer) |b| alloc.free(b);
    if (api_key.len > 0) {
        bearer = try std.fmt.allocPrint(alloc, "Bearer {s}", .{api_key});
        try list.append(.{ .name = "authorization", .value = bearer.? });
    }
    for (extra) |h| try list.append(h);
    const headers = try list.toOwnedSlice();
    return .{ .headers = headers, .bearer = bearer };
}

// Public for tests: build the JSON request body.
pub fn buildRequestJson(alloc: Allocator, req: CompletionRequest, streaming: bool) ![]u8 {
    var aw: Io.Writer.Allocating = .init(alloc);
    errdefer aw.deinit();
    var w: std.json.Stringify = .{ .writer = &aw.writer, .options = .{} };

    try w.beginObject();
    try w.objectField("model");
    try w.write(req.model);

    try w.objectField("messages");
    try writeMessages(&w, req.messages);

    if (req.tools.len > 0) {
        try w.objectField("tools");
        try writeTools(&w, req.tools);
    }

    if (req.temperature) |t| {
        try w.objectField("temperature");
        try w.write(t);
    }
    if (req.presence_penalty) |p| {
        try w.objectField("presence_penalty");
        try w.write(p);
    }
    if (req.max_tokens) |m| {
        try w.objectField("max_tokens");
        try w.write(m);
    }
    if (streaming) {
        try w.objectField("stream");
        try w.write(true);
        try w.objectField("stream_options");
        try w.beginObject();
        try w.objectField("include_usage");
        try w.write(true);
        try w.endObject();
    }

    try w.endObject();

    var al = aw.toArrayList();
    return al.toOwnedSlice(alloc);
}

fn writeMessages(w: *std.json.Stringify, msgs: []const Message) !void {
    try w.beginArray();
    for (msgs) |m| try writeMessage(w, m);
    try w.endArray();
}

fn writeMessage(w: *std.json.Stringify, m: Message) !void {
    switch (m) {
        .system => |text| {
            try w.beginObject();
            try w.objectField("role");
            try w.write("system");
            try w.objectField("content");
            try w.write(text);
            try w.endObject();
        },
        .user => |text| {
            try w.beginObject();
            try w.objectField("role");
            try w.write("user");
            try w.objectField("content");
            try w.write(text);
            try w.endObject();
        },
        .assistant => |a| {
            try w.beginObject();
            try w.objectField("role");
            try w.write("assistant");
            try w.objectField("content");
            if (a.text.len == 0 and a.tool_calls.len > 0) {
                try w.write(@as(?[]const u8, null));
            } else {
                try w.write(a.text);
            }
            if (a.tool_calls.len > 0) {
                try w.objectField("tool_calls");
                try w.beginArray();
                for (a.tool_calls) |tc| {
                    try w.beginObject();
                    try w.objectField("id");
                    try w.write(tc.id);
                    try w.objectField("type");
                    try w.write("function");
                    try w.objectField("function");
                    try w.beginObject();
                    try w.objectField("name");
                    try w.write(tc.name);
                    try w.objectField("arguments");
                    try w.write(tc.arguments_json);
                    try w.endObject();
                    try w.endObject();
                }
                try w.endArray();
            }
            try w.endObject();
        },
        .tool_result => |tr| {
            try w.beginObject();
            try w.objectField("role");
            try w.write("tool");
            try w.objectField("tool_call_id");
            try w.write(tr.call_id);
            try w.objectField("content");
            try w.write(tr.content);
            try w.endObject();
        },
        .summary => |text| {
            try w.beginObject();
            try w.objectField("role");
            try w.write("user");
            try w.objectField("content");
            try w.print("\"[Summary of earlier conversation]\\n{f}\"", .{std.zig.fmtString(text)});
            try w.endObject();
        },
    }
}

fn writeTools(w: *std.json.Stringify, tools: []const ToolDefinition) !void {
    try w.beginArray();
    for (tools) |t| {
        try w.beginObject();
        try w.objectField("type");
        try w.write("function");
        try w.objectField("function");
        try w.beginObject();
        try w.objectField("name");
        try w.write(t.name);
        try w.objectField("description");
        try w.write(t.description);
        try w.objectField("parameters");
        try w.beginObject();
        try w.objectField("type");
        try w.write("object");
        try w.objectField("properties");
        try w.beginObject();
        for (t.parameters) |p| {
            try w.objectField(p.name);
            try w.beginObject();
            try w.objectField("type");
            try w.write(p.type_name);
            try w.objectField("description");
            try w.write(p.description);
            try w.endObject();
        }
        try w.endObject();
        try w.objectField("required");
        try w.beginArray();
        for (t.parameters) |p| {
            if (p.required) try w.write(p.name);
        }
        try w.endArray();
        try w.endObject();
        try w.endObject();
        try w.endObject();
    }
    try w.endArray();
}

// ---------------------------------------------------------------------------
// Response parsing
// ---------------------------------------------------------------------------

const ApiFunctionRaw = struct {
    name: []const u8 = "",
    arguments: []const u8 = "",
};
const ApiToolCallRaw = struct {
    id: []const u8 = "",
    type: []const u8 = "function",
    function: ApiFunctionRaw = .{},
};
const ApiUsageRaw = struct {
    prompt_tokens: ?u32 = null,
    completion_tokens: ?u32 = null,
    total_tokens: ?u32 = null,
};
const ApiResponseMessageRaw = struct {
    content: ?[]const u8 = null,
    reasoning: ?[]const u8 = null,
    reasoning_content: ?[]const u8 = null,
    tool_calls: ?[]ApiToolCallRaw = null,
};
const ApiChoiceRaw = struct {
    message: ?ApiResponseMessageRaw = null,
};
const ApiResponseRaw = struct {
    choices: []ApiChoiceRaw = &.{},
    model: ?[]const u8 = null,
    usage: ?ApiUsageRaw = null,
};

// Returned CompletionResponse owns its strings; the caller frees them with
// the same allocator via freeResponse.
fn parseCompletion(alloc: Allocator, body: []const u8) LlmError!CompletionResponse {
    var parsed = std.json.parseFromSlice(ApiResponseRaw, alloc, body, .{
        .ignore_unknown_fields = true,
        .allocate = .alloc_always,
    }) catch return error.Json;
    defer parsed.deinit();

    if (parsed.value.choices.len == 0) return error.Api;
    const msg = parsed.value.choices[0].message orelse return error.Api;

    var calls = std.array_list.Managed(ToolCall).init(alloc);
    errdefer {
        for (calls.items) |c| {
            alloc.free(c.id);
            alloc.free(c.name);
            alloc.free(c.arguments_json);
        }
        calls.deinit();
    }

    if (msg.tool_calls) |tcs| {
        for (tcs) |tc| {
            const id_copy = alloc.dupe(u8, tc.id) catch return error.Json;
            errdefer alloc.free(id_copy);
            const name_copy = alloc.dupe(u8, tc.function.name) catch return error.Json;
            errdefer alloc.free(name_copy);
            const args_copy = alloc.dupe(u8, tc.function.arguments) catch return error.Json;
            errdefer alloc.free(args_copy);
            calls.append(.{ .id = id_copy, .name = name_copy, .arguments_json = args_copy }) catch return error.Json;
        }
    }

    const text_src = msg.content orelse "";
    const text_copy = alloc.dupe(u8, text_src) catch return error.Json;
    errdefer alloc.free(text_copy);

    const thinking_src: ?[]const u8 = msg.reasoning orelse msg.reasoning_content;
    const thinking_copy: ?[]const u8 = if (thinking_src) |t|
        if (t.len == 0) null else (alloc.dupe(u8, t) catch return error.Json)
    else
        null;
    errdefer if (thinking_copy) |t| alloc.free(t);

    const model_copy = alloc.dupe(u8, parsed.value.model orelse "") catch return error.Json;
    errdefer alloc.free(model_copy);

    const usage: Usage = if (parsed.value.usage) |u| .{
        .prompt_tokens = u.prompt_tokens orelse 0,
        .completion_tokens = u.completion_tokens orelse 0,
        .total_tokens = u.total_tokens orelse 0,
    } else .{};

    const calls_slice = calls.toOwnedSlice() catch return error.Json;

    return .{
        .text = text_copy,
        .thinking = thinking_copy,
        .tool_calls = calls_slice,
        .model = model_copy,
        .usage = usage,
    };
}

pub fn freeResponse(alloc: Allocator, r: *CompletionResponse) void {
    alloc.free(r.text);
    if (r.thinking) |t| alloc.free(t);
    for (r.tool_calls) |c| {
        alloc.free(c.id);
        alloc.free(c.name);
        alloc.free(c.arguments_json);
    }
    alloc.free(r.tool_calls);
    alloc.free(r.model);
}

const ApiModelRaw = struct {
    id: []const u8 = "",
    name: ?[]const u8 = null,
    context_length: ?u64 = null,
};
const ModelsResponseRaw = struct {
    data: []ApiModelRaw = &.{},
};

fn parseModels(alloc: Allocator, body: []const u8) LlmError![]ModelInfo {
    var parsed = std.json.parseFromSlice(ModelsResponseRaw, alloc, body, .{
        .ignore_unknown_fields = true,
        .allocate = .alloc_always,
    }) catch return error.Json;
    defer parsed.deinit();

    var out = std.array_list.Managed(ModelInfo).init(alloc);
    errdefer {
        for (out.items) |m| {
            alloc.free(m.id);
            alloc.free(m.name);
        }
        out.deinit();
    }

    for (parsed.value.data) |m| {
        const id_copy = alloc.dupe(u8, m.id) catch return error.Json;
        errdefer alloc.free(id_copy);
        const name_src = m.name orelse m.id;
        const name_copy = alloc.dupe(u8, name_src) catch return error.Json;
        errdefer alloc.free(name_copy);
        out.append(.{ .id = id_copy, .name = name_copy, .context_length = m.context_length }) catch return error.Json;
    }

    return out.toOwnedSlice() catch return error.Json;
}

// ---------------------------------------------------------------------------
// Streaming
// ---------------------------------------------------------------------------

const StreamFunctionDeltaRaw = struct {
    name: ?[]const u8 = null,
    arguments: ?[]const u8 = null,
};
const StreamToolCallDeltaRaw = struct {
    index: usize = 0,
    id: ?[]const u8 = null,
    function: ?StreamFunctionDeltaRaw = null,
};
const StreamDeltaMessageRaw = struct {
    content: ?[]const u8 = null,
    reasoning: ?[]const u8 = null,
    reasoning_content: ?[]const u8 = null,
    tool_calls: ?[]StreamToolCallDeltaRaw = null,
};
const StreamChoiceRaw = struct {
    delta: ?StreamDeltaMessageRaw = null,
    finish_reason: ?[]const u8 = null,
};
const StreamChunkRaw = struct {
    choices: []StreamChoiceRaw = &.{},
    usage: ?ApiUsageRaw = null,
};

const ToolCallBuf = struct {
    id: ?[]u8 = null,
    name: ?[]u8 = null,
    pending_args: std.array_list.Managed(u8),
    resolved_id: ?[]u8 = null,

    fn init(alloc: Allocator) ToolCallBuf {
        return .{ .pending_args = std.array_list.Managed(u8).init(alloc) };
    }
    fn deinit(self: *ToolCallBuf, alloc: Allocator) void {
        if (self.id) |s| alloc.free(s);
        if (self.name) |s| alloc.free(s);
        if (self.resolved_id) |s| alloc.free(s);
        self.pending_args.deinit();
    }
};

fn StreamState(comptime Ctx: type, comptime onEvent: fn (Ctx, StreamEvent) anyerror!void) type {
    return struct {
        const Self = @This();
        alloc: Allocator,
        ctx: Ctx,
        bufs: std.array_list.Managed(ToolCallBuf),
        last_usage: ?Usage,
        seen_done: bool,
        cb_err: ?anyerror,

        fn init(alloc: Allocator, ctx: Ctx) Self {
            return .{
                .alloc = alloc,
                .ctx = ctx,
                .bufs = std.array_list.Managed(ToolCallBuf).init(alloc),
                .last_usage = null,
                .seen_done = false,
                .cb_err = null,
            };
        }
        fn deinit(self: *Self) void {
            for (self.bufs.items) |*b| b.deinit(self.alloc);
            self.bufs.deinit();
        }

        fn emit(self: *Self, ev: StreamEvent) !void {
            onEvent(self.ctx, ev) catch |e| {
                self.cb_err = e;
                return error.Network;
            };
        }

        fn onSse(self: *Self, e: http.SseEvent) anyerror!void {
            if (self.cb_err != null) return;
            const data = e.data;
            if (std.mem.eql(u8, data, "[DONE]")) {
                self.seen_done = true;
                try self.emit(.{ .done = self.last_usage });
                return;
            }
            self.processChunk(data) catch |err| switch (err) {
                error.Network => return error.EndOfStream,
                else => return err,
            };
        }

        fn processChunk(self: *Self, data: []const u8) !void {
            var parsed = std.json.parseFromSlice(StreamChunkRaw, self.alloc, data, .{
                .ignore_unknown_fields = true,
                .allocate = .alloc_always,
            }) catch return;
            defer parsed.deinit();

            for (parsed.value.choices) |choice| {
                const delta = choice.delta orelse continue;
                const reasoning_src: ?[]const u8 = delta.reasoning orelse delta.reasoning_content;
                if (reasoning_src) |r| if (r.len > 0) try self.emit(.{ .reasoning = r });
                if (delta.content) |c| if (c.len > 0) try self.emit(.{ .delta = c });
                const tcs = delta.tool_calls orelse continue;
                for (tcs) |tc| try self.handleToolCall(tc);
            }

            if (parsed.value.usage) |u| {
                self.last_usage = .{
                    .prompt_tokens = u.prompt_tokens orelse 0,
                    .completion_tokens = u.completion_tokens orelse 0,
                    .total_tokens = u.total_tokens orelse 0,
                };
            }
        }

        fn handleToolCall(self: *Self, tc: StreamToolCallDeltaRaw) !void {
            const idx = tc.index;
            while (self.bufs.items.len <= idx) {
                try self.bufs.append(ToolCallBuf.init(self.alloc));
            }
            const buf = &self.bufs.items[idx];

            if (tc.id) |id| if (id.len > 0 and buf.id == null) {
                buf.id = try self.alloc.dupe(u8, id);
            };
            if (tc.function) |f| if (f.name) |name| if (name.len > 0 and buf.name == null) {
                buf.name = try self.alloc.dupe(u8, name);
            };

            if (buf.resolved_id == null) {
                if (buf.name) |name| {
                    const resolved = if (buf.id) |id|
                        try self.alloc.dupe(u8, id)
                    else
                        try std.fmt.allocPrint(self.alloc, "call_{d}", .{idx});
                    buf.resolved_id = resolved;
                    try self.emit(.{ .tool_call_start = .{ .id = resolved, .name = name } });
                    if (buf.pending_args.items.len > 0) {
                        try self.emit(.{ .tool_call_delta = .{ .id = resolved, .arguments = buf.pending_args.items } });
                        buf.pending_args.clearRetainingCapacity();
                    }
                }
            }

            const args = blk: {
                const f = tc.function orelse break :blk @as([]const u8, "");
                break :blk f.arguments orelse @as([]const u8, "");
            };
            if (args.len == 0) return;
            if (buf.resolved_id) |rid| {
                try self.emit(.{ .tool_call_delta = .{ .id = rid, .arguments = args } });
            } else {
                try buf.pending_args.appendSlice(args);
            }
        }

        fn finish(self: *Self) !void {
            if (self.cb_err) |e| return e;
            if (!self.seen_done) {
                onEvent(self.ctx, .{ .done = self.last_usage }) catch |e| return e;
            }
        }
    };
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

const testing = std.testing;

test "messages_to_api: system, user, assistant, tool_result, summary" {
    const calls = [_]ToolCall{
        .{ .id = "tc1", .name = "search", .arguments_json = "{\"q\":\"x\"}" },
    };
    const msgs = [_]Message{
        .{ .system = "be brief" },
        .{ .user = "hello" },
        .{ .assistant = .{ .text = "", .tool_calls = &calls, .thinking = null } },
        .{ .tool_result = .{ .call_id = "tc1", .content = "result", .is_error = false } },
        .{ .summary = "earlier" },
    };
    const req: CompletionRequest = .{ .model = "gpt-4", .messages = &msgs };
    const json = try buildRequestJson(testing.allocator, req, false);
    defer testing.allocator.free(json);

    const Parsed = struct {
        model: []const u8,
        messages: []std.json.Value,
    };
    var parsed = try std.json.parseFromSlice(Parsed, testing.allocator, json, .{ .ignore_unknown_fields = true });
    defer parsed.deinit();

    try testing.expectEqualStrings("gpt-4", parsed.value.model);
    try testing.expectEqual(@as(usize, 5), parsed.value.messages.len);

    try testing.expectEqualStrings("system", parsed.value.messages[0].object.get("role").?.string);
    try testing.expectEqualStrings("be brief", parsed.value.messages[0].object.get("content").?.string);

    try testing.expectEqualStrings("user", parsed.value.messages[1].object.get("role").?.string);

    const a = parsed.value.messages[2].object;
    try testing.expectEqualStrings("assistant", a.get("role").?.string);
    try testing.expectEqual(std.json.Value.null, a.get("content").?);
    try testing.expectEqual(@as(usize, 1), a.get("tool_calls").?.array.items.len);

    const tr = parsed.value.messages[3].object;
    try testing.expectEqualStrings("tool", tr.get("role").?.string);
    try testing.expectEqualStrings("tc1", tr.get("tool_call_id").?.string);
    try testing.expectEqualStrings("result", tr.get("content").?.string);

    const sm = parsed.value.messages[4].object;
    try testing.expectEqualStrings("user", sm.get("role").?.string);
    try testing.expect(std.mem.indexOf(u8, sm.get("content").?.string, "earlier") != null);
    try testing.expect(std.mem.startsWith(u8, sm.get("content").?.string, "[Summary of earlier conversation]"));
}

test "buildRequestJson includes stream_options when streaming" {
    const msgs = [_]Message{.{ .user = "hi" }};
    const req: CompletionRequest = .{ .model = "m", .messages = &msgs };
    const json = try buildRequestJson(testing.allocator, req, true);
    defer testing.allocator.free(json);
    try testing.expect(std.mem.indexOf(u8, json, "\"stream\":true") != null);
    try testing.expect(std.mem.indexOf(u8, json, "\"include_usage\":true") != null);
}

test "tools serialise with required list" {
    const params = [_]types.ToolParameter{
        .{ .name = "q", .type_name = "string", .description = "the query", .required = true },
        .{ .name = "n", .type_name = "number", .description = "limit", .required = false },
    };
    const tools = [_]ToolDefinition{
        .{ .name = "search", .description = "search the web", .parameters = &params },
    };
    const msgs = [_]Message{.{ .user = "hi" }};
    const req: CompletionRequest = .{ .model = "m", .messages = &msgs, .tools = &tools };
    const json = try buildRequestJson(testing.allocator, req, false);
    defer testing.allocator.free(json);

    var parsed = try std.json.parseFromSlice(std.json.Value, testing.allocator, json, .{});
    defer parsed.deinit();

    const tool0 = parsed.value.object.get("tools").?.array.items[0].object;
    try testing.expectEqualStrings("function", tool0.get("type").?.string);
    const func = tool0.get("function").?.object;
    try testing.expectEqualStrings("search", func.get("name").?.string);
    const schema = func.get("parameters").?.object;
    try testing.expectEqualStrings("object", schema.get("type").?.string);
    try testing.expectEqualStrings("string", schema.get("properties").?.object.get("q").?.object.get("type").?.string);
    const required = schema.get("required").?.array;
    try testing.expectEqual(@as(usize, 1), required.items.len);
    try testing.expectEqualStrings("q", required.items[0].string);
}

test "parseCompletion: text + tool_call + usage" {
    const body =
        \\{"model":"m1","choices":[{"message":{"content":"hi","tool_calls":[
        \\  {"id":"t1","type":"function","function":{"name":"f","arguments":"{}"}}
        \\]}}],"usage":{"prompt_tokens":3,"completion_tokens":2,"total_tokens":5}}
    ;
    var resp = try parseCompletion(testing.allocator, body);
    defer freeResponse(testing.allocator, &resp);
    try testing.expectEqualStrings("hi", resp.text);
    try testing.expectEqualStrings("m1", resp.model);
    try testing.expectEqual(@as(u32, 5), resp.usage.total_tokens);
    try testing.expectEqual(@as(usize, 1), resp.tool_calls.len);
    try testing.expectEqualStrings("t1", resp.tool_calls[0].id);
    try testing.expectEqualStrings("f", resp.tool_calls[0].name);
}

test "parseCompletion: reasoning_content surfaces as thinking" {
    const body =
        \\{"choices":[{"message":{"content":"x","reasoning_content":"because"}}]}
    ;
    var resp = try parseCompletion(testing.allocator, body);
    defer freeResponse(testing.allocator, &resp);
    try testing.expectEqualStrings("because", resp.thinking.?);
}

const EventCollector = struct {
    list: std.array_list.Managed(OwnedEvent),
    alloc: Allocator,

    const OwnedEvent = union(enum) {
        reasoning: []u8,
        delta: []u8,
        tool_call_start: struct { id: []u8, name: []u8 },
        tool_call_delta: struct { id: []u8, arguments: []u8 },
        done: ?Usage,
    };

    fn init(a: Allocator) EventCollector {
        return .{ .list = std.array_list.Managed(OwnedEvent).init(a), .alloc = a };
    }
    fn deinit(self: *EventCollector) void {
        for (self.list.items) |ev| switch (ev) {
            .reasoning => |s| self.alloc.free(s),
            .delta => |s| self.alloc.free(s),
            .tool_call_start => |t| {
                self.alloc.free(t.id);
                self.alloc.free(t.name);
            },
            .tool_call_delta => |t| {
                self.alloc.free(t.id);
                self.alloc.free(t.arguments);
            },
            .done => {},
        };
        self.list.deinit();
    }
    fn cb(self: *EventCollector, ev: StreamEvent) anyerror!void {
        const owned: OwnedEvent = switch (ev) {
            .reasoning => |s| .{ .reasoning = try self.alloc.dupe(u8, s) },
            .delta => |s| .{ .delta = try self.alloc.dupe(u8, s) },
            .tool_call_start => |t| .{ .tool_call_start = .{
                .id = try self.alloc.dupe(u8, t.id),
                .name = try self.alloc.dupe(u8, t.name),
            } },
            .tool_call_delta => |t| .{ .tool_call_delta = .{
                .id = try self.alloc.dupe(u8, t.id),
                .arguments = try self.alloc.dupe(u8, t.arguments),
            } },
            .done => |u| .{ .done = u },
        };
        try self.list.append(owned);
    }
};

fn TestState() type {
    return StreamState(*EventCollector, EventCollector.cb);
}

test "stream: content delta then DONE emits delta + done" {
    var c = EventCollector.init(testing.allocator);
    defer c.deinit();
    var st = TestState().init(testing.allocator, &c);
    defer st.deinit();
    try st.onSse(.{ .event = null, .data = "{\"choices\":[{\"delta\":{\"content\":\"hello\"}}]}" });
    try st.onSse(.{ .event = null, .data = "[DONE]" });
    try st.finish();
    try testing.expectEqual(@as(usize, 2), c.list.items.len);
    try testing.expectEqualStrings("hello", c.list.items[0].delta);
    try testing.expect(c.list.items[1] == .done);
}

test "stream: tool_call start then args delta" {
    var c = EventCollector.init(testing.allocator);
    defer c.deinit();
    var st = TestState().init(testing.allocator, &c);
    defer st.deinit();
    try st.onSse(.{ .event = null, .data =
        \\{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"tc-1","function":{"name":"web_search","arguments":""}}]}}]}
    });
    try st.onSse(.{ .event = null, .data =
        \\{"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"{\"q\":\"r\"}"}}]}}]}
    });
    try st.onSse(.{ .event = null, .data = "[DONE]" });
    try st.finish();

    try testing.expectEqual(@as(usize, 3), c.list.items.len);
    try testing.expectEqualStrings("tc-1", c.list.items[0].tool_call_start.id);
    try testing.expectEqualStrings("web_search", c.list.items[0].tool_call_start.name);
    try testing.expectEqualStrings("tc-1", c.list.items[1].tool_call_delta.id);
    try testing.expect(std.mem.indexOf(u8, c.list.items[1].tool_call_delta.arguments, "r") != null);
}

test "stream: args-before-name buffers and flushes" {
    var c = EventCollector.init(testing.allocator);
    defer c.deinit();
    var st = TestState().init(testing.allocator, &c);
    defer st.deinit();
    try st.onSse(.{ .event = null, .data =
        \\{"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"{\"a\":"}}]}}]}
    });
    try st.onSse(.{ .event = null, .data =
        \\{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"t1","function":{"name":"f","arguments":"1}"}}]}}]}
    });
    try st.onSse(.{ .event = null, .data = "[DONE]" });
    try st.finish();

    try testing.expectEqual(@as(usize, 4), c.list.items.len);
    try testing.expectEqualStrings("t1", c.list.items[0].tool_call_start.id);
    try testing.expectEqualStrings("f", c.list.items[0].tool_call_start.name);
    try testing.expectEqualStrings("{\"a\":", c.list.items[1].tool_call_delta.arguments);
    try testing.expectEqualStrings("1}", c.list.items[2].tool_call_delta.arguments);
    try testing.expect(c.list.items[3] == .done);
}

test "stream: missing id synthesises call_<index>" {
    var c = EventCollector.init(testing.allocator);
    defer c.deinit();
    var st = TestState().init(testing.allocator, &c);
    defer st.deinit();
    try st.onSse(.{ .event = null, .data =
        \\{"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"name":"f","arguments":"{}"}}]}}]}
    });
    try st.onSse(.{ .event = null, .data = "[DONE]" });
    try st.finish();

    try testing.expectEqual(@as(usize, 3), c.list.items.len);
    try testing.expectEqualStrings("call_0", c.list.items[0].tool_call_start.id);
}

test "stream: usage chunk surfaces in final done" {
    var c = EventCollector.init(testing.allocator);
    defer c.deinit();
    var st = TestState().init(testing.allocator, &c);
    defer st.deinit();
    try st.onSse(.{ .event = null, .data =
        \\{"choices":[{"delta":{"content":"x"}}],"usage":{"prompt_tokens":1,"completion_tokens":2,"total_tokens":3}}
    });
    try st.onSse(.{ .event = null, .data = "[DONE]" });
    try st.finish();

    try testing.expectEqual(@as(usize, 2), c.list.items.len);
    const u = c.list.items[1].done.?;
    try testing.expectEqual(@as(u32, 3), u.total_tokens);
}

test "stream: reasoning_content emits reasoning event" {
    var c = EventCollector.init(testing.allocator);
    defer c.deinit();
    var st = TestState().init(testing.allocator, &c);
    defer st.deinit();
    try st.onSse(.{ .event = null, .data =
        \\{"choices":[{"delta":{"reasoning_content":"think"}}]}
    });
    try st.onSse(.{ .event = null, .data = "[DONE]" });
    try st.finish();

    try testing.expectEqual(@as(usize, 2), c.list.items.len);
    try testing.expectEqualStrings("think", c.list.items[0].reasoning);
}

test "parseModels: id and name fallback" {
    const body =
        \\{"data":[{"id":"gpt-4","name":"GPT-4","context_length":128000},{"id":"only-id"}]}
    ;
    const list = try parseModels(testing.allocator, body);
    defer {
        for (list) |m| {
            testing.allocator.free(m.id);
            testing.allocator.free(m.name);
        }
        testing.allocator.free(list);
    }
    try testing.expectEqual(@as(usize, 2), list.len);
    try testing.expectEqualStrings("gpt-4", list[0].id);
    try testing.expectEqualStrings("GPT-4", list[0].name);
    try testing.expectEqual(@as(?u64, 128000), list[0].context_length);
    try testing.expectEqualStrings("only-id", list[1].id);
    try testing.expectEqualStrings("only-id", list[1].name);
}
