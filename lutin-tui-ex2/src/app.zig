const std = @import("std");
const vaxis = @import("vaxis");

const model_mod = @import("model/model.zig");
const update = @import("input/update.zig");
const view = @import("view/view.zig");
const theme = @import("ui/theme.zig");
const types = @import("llm/types.zig");
const runner_mod = @import("agent/runner.zig");
const reducer = @import("agent/reducer.zig");
const settings_mod = @import("config/settings.zig");

const Model = model_mod.Model;

pub const AgentEventEnvelope = struct {
    tab_id: u32,
    event: types.StreamEvent,
};

pub const AgentTurnEnvelope = struct {
    tab_id: u32,
    message: types.Message,
};

pub const AgentErrorEnvelope = struct {
    tab_id: u32,
    message: []const u8,
};

pub const Event = union(enum) {
    key_press: vaxis.Key,
    winsize: vaxis.Winsize,
    focus_in,
    focus_out,
    paste_start,
    paste_end,
    agent_event: AgentEventEnvelope,
    agent_turn_done: AgentTurnEnvelope,
    agent_run_done: u32,
    agent_error: AgentErrorEnvelope,
};

const PostCtx = struct {
    loop: *vaxis.Loop(Event),
    gpa: std.mem.Allocator,
};

// WHY: the agent worker runs on its own thread; vaxis's Loop queue is the only
// shared channel the main thread blocks on, so we post events back via postEvent.
fn postFromWorker(ctx_any: *anyopaque, tab_id: u32, ob: runner_mod.Outbound) void {
    const ctx: *PostCtx = @ptrCast(@alignCast(ctx_any));
    const ev: Event = switch (ob) {
        .event => |e| .{ .agent_event = .{ .tab_id = tab_id, .event = e } },
        .turn_done => |m| .{ .agent_turn_done = .{ .tab_id = tab_id, .message = m } },
        .run_done => .{ .agent_run_done = tab_id },
        .err => |s| .{ .agent_error = .{ .tab_id = tab_id, .message = s } },
    };
    // WHY: postEvent may drop on a full queue; we accepted ownership of the
    // payload from the worker so we must free it ourselves on drop or leak.
    ctx.loop.postEvent(ev) catch runner_mod.freeOutbound(ctx.gpa, ob);
}

pub const Ctx = struct {
    alloc: std.mem.Allocator,
    io: std.Io,
    env: *std.process.Environ.Map,
    loop: *vaxis.Loop(Event),
    settings: settings_mod.Settings,
    runners: std.AutoHashMap(u32, *runner_mod.Runner),
    post_ctx: *PostCtx,

    pub fn deinit(self: *Ctx) void {
        var it = self.runners.valueIterator();
        while (it.next()) |r| {
            r.*.requestCancel();
            r.*.joinAndFree();
        }
        self.runners.deinit();
        self.alloc.destroy(self.post_ctx);
    }

    pub fn spawnRunner(self: *Ctx, tab_id: u32, transcript: []const types.Message) !void {
        if (self.runners.get(tab_id)) |old| {
            old.requestCancel();
            old.joinAndFree();
            _ = self.runners.remove(tab_id);
        }
        const api_key = settings_mod.resolveApiKey(self.env, self.settings.api_key_env);
        const r = try runner_mod.Runner.start(self.alloc, self.io, tab_id, transcript, .{
            .base_url = self.settings.base_url,
            .api_key = api_key,
            .model = self.settings.model,
            .persona = self.settings.persona,
            .temperature = self.settings.temperature,
            .max_tokens = self.settings.max_tokens,
        }, @ptrCast(self.post_ctx), postFromWorker);
        try self.runners.put(tab_id, r);
    }
};

pub fn run(io: std.Io, gpa: std.mem.Allocator, env_map: *std.process.Environ.Map) !void {
    var tty_buf: [4096]u8 = undefined;
    var tty = try vaxis.Tty.init(io, &tty_buf);
    defer tty.deinit();

    var vx = try vaxis.init(io, gpa, env_map, .{
        .system_clipboard_allocator = gpa,
    });
    defer vx.deinit(gpa, tty.writer());

    var loop: vaxis.Loop(Event) = .init(io, &tty, &vx);
    try loop.installResizeHandler();
    try loop.start();
    defer loop.stop();

    try vx.enterAltScreen(tty.writer());
    try vx.queryTerminal(tty.writer(), .fromSeconds(1));

    var model = try Model.init(gpa);
    defer model.deinit();

    var settings_loaded = settings_mod.loadOrCreate(io, gpa, env_map) catch |err| {
        const msg = try std.fmt.allocPrint(gpa, "settings load failed: {s}", .{@errorName(err)});
        defer gpa.free(msg);
        try model.flash.set(gpa, msg, std.Io.Clock.awake.now(io).toMilliseconds(), 5_000);
        return runWithoutAgent(io, gpa, env_map, &tty, &vx, &loop, &model);
    };
    defer settings_loaded.deinit(gpa);

    const post_ctx = try gpa.create(PostCtx);
    post_ctx.* = .{ .loop = &loop, .gpa = gpa };

    var ctx: Ctx = .{
        .alloc = gpa,
        .io = io,
        .env = env_map,
        .loop = &loop,
        .settings = settings_loaded.value,
        .runners = std.AutoHashMap(u32, *runner_mod.Runner).init(gpa),
        .post_ctx = post_ctx,
    };
    defer ctx.deinit();

    if (settings_loaded.was_created) {
        try model.flash.set(gpa, "edit ~/.config/lutin/settings.json and restart", std.Io.Clock.awake.now(io).toMilliseconds(), 8_000);
    }

    try eventLoop(io, gpa, &tty, &vx, &loop, &model, &ctx);
}

fn runWithoutAgent(
    io: std.Io,
    gpa: std.mem.Allocator,
    env_map: *std.process.Environ.Map,
    tty: *vaxis.Tty,
    vx: *vaxis.Vaxis,
    loop: *vaxis.Loop(Event),
    model: *Model,
) !void {
    _ = env_map;
    const th = theme.slate;
    while (!model.quit) {
        if (model.dirty) {
            model.dirty = false;
            const win = vx.window();
            win.hideCursor();
            view.render(win, model, std.Io.Clock.awake.now(io).toMilliseconds(), th);
            try vx.render(tty.writer());
        }
        const evt = try loop.nextEvent();
        switch (evt) {
            .key_press => |k| try update.onKey(model, k, std.Io.Clock.awake.now(io).toMilliseconds(), null),
            .winsize => |ws| try vx.resize(gpa, tty.writer(), ws),
            else => {},
        }
        model.dirty = true;
    }
}

fn eventLoop(
    io: std.Io,
    gpa: std.mem.Allocator,
    tty: *vaxis.Tty,
    vx: *vaxis.Vaxis,
    loop: *vaxis.Loop(Event),
    model: *Model,
    ctx: *Ctx,
) !void {
    _ = loop;
    const th = theme.slate;
    while (!model.quit) {
        if (model.dirty) {
            model.dirty = false;
            const win = vx.window();
            win.hideCursor();
            view.render(win, model, std.Io.Clock.awake.now(io).toMilliseconds(), th);
            try vx.render(tty.writer());
        }

        const evt = try ctx.loop.nextEvent();
        const now_ms = std.Io.Clock.awake.now(io).toMilliseconds();
        switch (evt) {
            .key_press => |k| try update.onKey(model, k, now_ms, ctx),
            .winsize => |ws| try vx.resize(gpa, tty.writer(), ws),
            .agent_event => |env| {
                defer runner_mod.freeStreamEvent(gpa, env.event);
                if (findTab(model, env.tab_id)) |tab| {
                    tab.indicator = .running;
                    reducer.applyStreamEvent(gpa, tab, env.event) catch {};
                }
            },
            .agent_turn_done => |env| {
                if (findTab(model, env.tab_id)) |tab| {
                    reducer.discardStreaming(gpa, tab);
                    tab.messages.append(gpa, env.message) catch {
                        var m = env.message;
                        @import("agent/loop.zig").freeMessage(gpa, &m);
                    };
                } else {
                    var m = env.message;
                    @import("agent/loop.zig").freeMessage(gpa, &m);
                }
            },
            .agent_run_done => |tab_id| {
                if (findTab(model, tab_id)) |tab| {
                    tab.indicator = .completed;
                }
                if (ctx.runners.get(tab_id)) |r| {
                    r.joinAndFree();
                    _ = ctx.runners.remove(tab_id);
                }
            },
            .agent_error => |env| {
                defer gpa.free(env.message);
                model.flash.set(gpa, env.message, now_ms, 5_000) catch {};
            },
            else => {},
        }
        model.dirty = true;
    }
}

fn findTab(model: *Model, tab_id: u32) ?*model_mod.Tab {
    for (model.tabs.items) |*t| {
        if (t.id == tab_id) return t;
    }
    return null;
}
