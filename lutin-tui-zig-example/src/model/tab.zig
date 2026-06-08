const std = @import("std");
const nav = @import("nav.zig");
const list_widget = @import("../ui/list.zig");
const types = @import("../llm/types.zig");
const loop = @import("../agent/loop.zig");

pub const PaneFocus = enum { chat, rail, composer };

pub const RailCard = enum { persona, subagents, session, project, metrics };

pub const InFlightCall = struct {
    id: std.ArrayList(u8) = .empty,
    name: std.ArrayList(u8) = .empty,
    arguments: std.ArrayList(u8) = .empty,

    pub fn deinit(self: *InFlightCall, alloc: std.mem.Allocator) void {
        self.id.deinit(alloc);
        self.name.deinit(alloc);
        self.arguments.deinit(alloc);
    }
};

pub const StreamingBuf = struct {
    text: std.ArrayList(u8) = .empty,
    thinking: std.ArrayList(u8) = .empty,
    tool_calls: std.ArrayList(InFlightCall) = .empty,

    pub fn deinit(self: *StreamingBuf, alloc: std.mem.Allocator) void {
        self.text.deinit(alloc);
        self.thinking.deinit(alloc);
        for (self.tool_calls.items) |*c| c.deinit(alloc);
        self.tool_calls.deinit(alloc);
    }

    pub fn findCall(self: *StreamingBuf, id: []const u8) ?*InFlightCall {
        for (self.tool_calls.items) |*c| {
            if (std.mem.eql(u8, c.id.items, id)) return c;
        }
        return null;
    }
};

pub const Tab = struct {
    id: u32,
    title: std.ArrayList(u8),
    session_id: ?u64 = null,
    stack: nav.Stack,
    pane: PaneFocus = .chat,

    chat_list: list_widget.ListState = .{},
    rail_list: list_widget.ListState = .{},

    indicator: Indicator = .none,
    rail_visible: bool = true,

    messages: std.ArrayList(types.Message) = .empty,
    streaming: ?StreamingBuf = null,

    pub const Indicator = enum { none, running, approval, completed };

    pub fn init(alloc: std.mem.Allocator, id: u32, title: []const u8) !Tab {
        var t: Tab = .{
            .id = id,
            .title = .empty,
            .stack = try nav.Stack.withRoot(alloc, .{ .kind = .main, .title = "main" }),
        };
        try t.title.appendSlice(alloc, title);
        return t;
    }

    pub fn deinit(self: *Tab, alloc: std.mem.Allocator) void {
        self.title.deinit(alloc);
        self.stack.deinit(alloc);
        for (self.messages.items) |*m| loop.freeMessage(alloc, m);
        self.messages.deinit(alloc);
        if (self.streaming) |*s| s.deinit(alloc);
    }

    pub fn rename(self: *Tab, alloc: std.mem.Allocator, title: []const u8) !void {
        self.title.clearRetainingCapacity();
        try self.title.appendSlice(alloc, title);
    }
};
