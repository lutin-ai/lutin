const std = @import("std");
const types = @import("../llm/types.zig");
const tab_mod = @import("../model/tab.zig");

const Tab = tab_mod.Tab;
const StreamingBuf = tab_mod.StreamingBuf;
const InFlightCall = tab_mod.InFlightCall;

pub fn ensureStreaming(tab: *Tab) *StreamingBuf {
    if (tab.streaming == null) tab.streaming = .{};
    return &tab.streaming.?;
}

pub fn applyStreamEvent(
    alloc: std.mem.Allocator,
    tab: *Tab,
    ev: types.StreamEvent,
) !void {
    const buf = ensureStreaming(tab);
    switch (ev) {
        .reasoning => |s| try buf.thinking.appendSlice(alloc, s),
        .delta => |s| try buf.text.appendSlice(alloc, s),
        .tool_call_start => |t| {
            if (buf.findCall(t.id) == null) {
                var c: InFlightCall = .{};
                try c.id.appendSlice(alloc, t.id);
                try c.name.appendSlice(alloc, t.name);
                try buf.tool_calls.append(alloc, c);
            }
        },
        .tool_call_delta => |t| {
            if (buf.findCall(t.id)) |c| {
                try c.arguments.appendSlice(alloc, t.arguments);
            } else {
                var c: InFlightCall = .{};
                try c.id.appendSlice(alloc, t.id);
                try c.arguments.appendSlice(alloc, t.arguments);
                try buf.tool_calls.append(alloc, c);
            }
        },
        .done => {},
    }
}

pub fn discardStreaming(alloc: std.mem.Allocator, tab: *Tab) void {
    if (tab.streaming) |*buf| buf.deinit(alloc);
    tab.streaming = null;
}

pub fn appendToolResult(
    alloc: std.mem.Allocator,
    tab: *Tab,
    call_id: []const u8,
    content: []const u8,
    is_error: bool,
) !void {
    const id_dup = try alloc.dupe(u8, call_id);
    errdefer alloc.free(id_dup);
    const c_dup = try alloc.dupe(u8, content);
    try tab.messages.append(alloc, .{ .tool_result = .{
        .call_id = id_dup,
        .content = c_dup,
        .is_error = is_error,
    } });
}

const testing = std.testing;

test "reducer: deltas accumulate into streaming buffer, discard clears it" {
    const alloc = testing.allocator;
    var tab = try Tab.init(alloc, 1, "t");
    defer tab.deinit(alloc);

    try applyStreamEvent(alloc, &tab, .{ .delta = "hello " });
    try applyStreamEvent(alloc, &tab, .{ .delta = "world" });
    try applyStreamEvent(alloc, &tab, .{ .tool_call_start = .{ .id = "c1", .name = "echo" } });
    try applyStreamEvent(alloc, &tab, .{ .tool_call_delta = .{ .id = "c1", .arguments = "{\"x\":" } });
    try applyStreamEvent(alloc, &tab, .{ .tool_call_delta = .{ .id = "c1", .arguments = "1}" } });
    try applyStreamEvent(alloc, &tab, .{ .done = null });

    try testing.expect(tab.streaming != null);
    try testing.expectEqualStrings("hello world", tab.streaming.?.text.items);
    try testing.expectEqual(@as(usize, 1), tab.streaming.?.tool_calls.items.len);
    try testing.expectEqualStrings("{\"x\":1}", tab.streaming.?.tool_calls.items[0].arguments.items);

    discardStreaming(alloc, &tab);
    try testing.expect(tab.streaming == null);
    try testing.expectEqual(@as(usize, 0), tab.messages.items.len);

    const text = try alloc.dupe(u8, "hi");
    try tab.messages.append(alloc, .{ .assistant = .{
        .text = text,
        .tool_calls = &.{},
        .thinking = null,
    } });
    try testing.expectEqual(@as(usize, 1), tab.messages.items.len);
    try testing.expect(tab.messages.items[0] == .assistant);
    try testing.expectEqualStrings("hi", tab.messages.items[0].assistant.text);
}
