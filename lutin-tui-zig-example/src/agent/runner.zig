const std = @import("std");
const types = @import("../llm/types.zig");
const provider_mod = @import("../llm/openai_compat.zig");
const loop = @import("loop.zig");
const approval = @import("approval.zig");

pub const ProviderConfig = struct {
    base_url: []const u8,
    api_key: []const u8,
    model: []const u8,
    persona: []const u8,
    temperature: ?f32 = null,
    max_tokens: ?u32 = null,
};

pub const Outbound = union(enum) {
    event: types.StreamEvent,
    turn_done: types.Message,
    run_done,
    err: []const u8,
};

pub const PostFn = *const fn (ctx: *anyopaque, tab_id: u32, msg: Outbound) void;

pub fn freeOutbound(alloc: std.mem.Allocator, ob: Outbound) void {
    switch (ob) {
        .event => |e| freeStreamEvent(alloc, e),
        .turn_done => |m| {
            var mm = m;
            loop.freeMessage(alloc, &mm);
        },
        .run_done => {},
        .err => |s| alloc.free(s),
    }
}

pub const Runner = struct {
    alloc: std.mem.Allocator,
    io: std.Io,
    tab_id: u32,
    transcript: std.ArrayList(types.Message),
    cfg_owned: OwnedConfig,
    post_ctx: *anyopaque,
    post: PostFn,
    cancel: std.atomic.Value(bool) = .init(false),
    future: ?std.Io.Future(void) = null,

    const OwnedConfig = struct {
        base_url: []u8,
        api_key: []u8,
        model: []u8,
        persona: []u8,
        temperature: ?f32,
        max_tokens: ?u32,

        fn deinit(self: *OwnedConfig, alloc: std.mem.Allocator) void {
            alloc.free(self.base_url);
            alloc.free(self.api_key);
            alloc.free(self.model);
            alloc.free(self.persona);
        }
    };

    pub fn start(
        alloc: std.mem.Allocator,
        io: std.Io,
        tab_id: u32,
        transcript_src: []const types.Message,
        cfg: ProviderConfig,
        post_ctx: *anyopaque,
        post: PostFn,
    ) !*Runner {
        const r = try alloc.create(Runner);
        errdefer alloc.destroy(r);

        var owned: std.ArrayList(types.Message) = .empty;
        errdefer {
            for (owned.items) |*m| loop.freeMessage(alloc, m);
            owned.deinit(alloc);
        }
        for (transcript_src) |m| try owned.append(alloc, try dupMessage(alloc, m));

        const oc: OwnedConfig = .{
            .base_url = try alloc.dupe(u8, cfg.base_url),
            .api_key = try alloc.dupe(u8, cfg.api_key),
            .model = try alloc.dupe(u8, cfg.model),
            .persona = try alloc.dupe(u8, cfg.persona),
            .temperature = cfg.temperature,
            .max_tokens = cfg.max_tokens,
        };

        r.* = .{
            .alloc = alloc,
            .io = io,
            .tab_id = tab_id,
            .transcript = owned,
            .cfg_owned = oc,
            .post_ctx = post_ctx,
            .post = post,
        };

        r.future = try io.concurrent(threadMain, .{r});
        return r;
    }

    pub fn requestCancel(self: *Runner) void {
        self.cancel.store(true, .seq_cst);
    }

    pub fn joinAndFree(self: *Runner) void {
        if (self.future) |*f| _ = f.await(self.io);
        self.future = null;
        for (self.transcript.items) |*m| loop.freeMessage(self.alloc, m);
        self.transcript.deinit(self.alloc);
        self.cfg_owned.deinit(self.alloc);
        self.alloc.destroy(self);
    }

    fn threadMain(self: *Runner) void {
        const alloc = self.alloc;

        var provider: provider_mod.Provider = .{
            .alloc = alloc,
            .io = self.io,
            .base_url = self.cfg_owned.base_url,
            .api_key = self.cfg_owned.api_key,
        };

        var policy = approval.Policy.empty(alloc);
        defer policy.deinit();

        const file_tools_mod = @import("../tools/file.zig");
        var file_ctx: file_tools_mod.FileToolCtx = .{ .io = self.io };
        var tools_buf: []loop.ToolExec.Tool = &.{};
        var tools_owned = false;
        if (file_tools_mod.allFileTools(alloc, &file_ctx)) |t| {
            tools_buf = t;
            tools_owned = true;
        } else |_| {}
        defer if (tools_owned) file_tools_mod.freeTools(alloc, tools_buf);

        const hooks: loop.Hooks = .{
            .ctx = @ptrCast(self),
            .ask_approval = askApproval,
            .on_event = onEvent,
            .on_turn = onTurn,
        };

        loop.run(alloc, &self.transcript, tools_buf, &provider, &policy, hooks, .{
            .model = self.cfg_owned.model,
            .persona = self.cfg_owned.persona,
            .temperature = self.cfg_owned.temperature,
            .max_tokens = self.cfg_owned.max_tokens,
        }) catch |err| {
            const msg = if (provider.last_error) |detail|
                std.fmt.allocPrint(alloc, "agent error: {s}: {s}", .{ @errorName(err), detail }) catch null
            else
                std.fmt.allocPrint(alloc, "agent error: {s}", .{@errorName(err)}) catch null;
            if (msg) |m| {
                self.post(self.post_ctx, self.tab_id, .{ .err = m });
            }
            provider.deinit();
            self.post(self.post_ctx, self.tab_id, .run_done);
            return;
        };

        provider.deinit();
        self.post(self.post_ctx, self.tab_id, .run_done);
    }

    fn onEvent(ctx: *anyopaque, ev: types.StreamEvent) anyerror!void {
        const self: *Runner = @ptrCast(@alignCast(ctx));
        if (self.cancel.load(.seq_cst)) return error.Canceled;
        const owned = try cloneStreamEvent(self.alloc, ev);
        self.post(self.post_ctx, self.tab_id, .{ .event = owned });
    }

    // Contract: the worker already owns the message (it is borrowed from its
    // transcript). We clone it so the UI gets a fresh owned copy and the
    // worker's transcript copy remains intact.
    fn onTurn(ctx: *anyopaque, msg: types.Message) anyerror!void {
        const self: *Runner = @ptrCast(@alignCast(ctx));
        const cloned = try dupMessage(self.alloc, msg);
        self.post(self.post_ctx, self.tab_id, .{ .turn_done = cloned });
    }

    // First-pass approval: always deny, surface a flash via the err channel.
    // TODO: route approval requests as their own variant so the UI overlay can answer.
    fn askApproval(ctx: *anyopaque, req: loop.ApprovalRequest) anyerror!loop.ApprovalAnswer {
        const self: *Runner = @ptrCast(@alignCast(ctx));
        const msg = std.fmt.allocPrint(
            self.alloc,
            "approval not yet wired through (tool={s})",
            .{req.tool},
        ) catch return .deny_once;
        self.post(self.post_ctx, self.tab_id, .{ .err = msg });
        return .deny_once;
    }
};

pub fn dupMessage(alloc: std.mem.Allocator, m: types.Message) !types.Message {
    return switch (m) {
        .system => |s| .{ .system = try alloc.dupe(u8, s) },
        .user => |s| .{ .user = try alloc.dupe(u8, s) },
        .summary => |s| .{ .summary = try alloc.dupe(u8, s) },
        .assistant => |a| blk: {
            const text = try alloc.dupe(u8, a.text);
            errdefer alloc.free(text);
            const thinking: ?[]const u8 = if (a.thinking) |t| try alloc.dupe(u8, t) else null;
            errdefer if (thinking) |t| alloc.free(t);
            const calls = try alloc.alloc(types.ToolCall, a.tool_calls.len);
            var filled: usize = 0;
            errdefer {
                for (calls[0..filled]) |c| {
                    alloc.free(c.id);
                    alloc.free(c.name);
                    alloc.free(c.arguments_json);
                }
                alloc.free(calls);
            }
            for (a.tool_calls, 0..) |c, i| {
                const id = try alloc.dupe(u8, c.id);
                errdefer alloc.free(id);
                const name = try alloc.dupe(u8, c.name);
                errdefer alloc.free(name);
                const args = try alloc.dupe(u8, c.arguments_json);
                calls[i] = .{ .id = id, .name = name, .arguments_json = args };
                filled = i + 1;
            }
            break :blk .{ .assistant = .{ .text = text, .tool_calls = calls, .thinking = thinking } };
        },
        .tool_result => |tr| .{ .tool_result = .{
            .call_id = try alloc.dupe(u8, tr.call_id),
            .content = try alloc.dupe(u8, tr.content),
            .is_error = tr.is_error,
        } },
    };
}

pub fn cloneStreamEvent(alloc: std.mem.Allocator, ev: types.StreamEvent) !types.StreamEvent {
    return switch (ev) {
        .reasoning => |s| .{ .reasoning = try alloc.dupe(u8, s) },
        .delta => |s| .{ .delta = try alloc.dupe(u8, s) },
        .tool_call_start => |t| .{ .tool_call_start = .{
            .id = try alloc.dupe(u8, t.id),
            .name = try alloc.dupe(u8, t.name),
        } },
        .tool_call_delta => |t| .{ .tool_call_delta = .{
            .id = try alloc.dupe(u8, t.id),
            .arguments = try alloc.dupe(u8, t.arguments),
        } },
        .done => |u| .{ .done = u },
    };
}

pub fn freeStreamEvent(alloc: std.mem.Allocator, ev: types.StreamEvent) void {
    switch (ev) {
        .reasoning => |s| alloc.free(s),
        .delta => |s| alloc.free(s),
        .tool_call_start => |t| {
            alloc.free(t.id);
            alloc.free(t.name);
        },
        .tool_call_delta => |t| {
            alloc.free(t.id);
            alloc.free(t.arguments);
        },
        .done => {},
    }
}
