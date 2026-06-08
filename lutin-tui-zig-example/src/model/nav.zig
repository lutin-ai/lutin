const std = @import("std");

pub const ViewKind = enum {
    main,
    dashboard,
    message_detail,
    tool_call_detail,
    subagent_detail,
    settings,
    help,
};

pub const View = struct {
    kind: ViewKind,
    title: []const u8,
    payload: Payload = .none,

    pub const Payload = union(enum) {
        none,
        message_id: u64,
        tool_call_id: u64,
        subagent_id: u64,
        settings_section: u8,
    };
};

pub const Stack = struct {
    items: std.ArrayList(View),

    pub fn init() Stack {
        return .{ .items = .empty };
    }

    pub fn deinit(self: *Stack, alloc: std.mem.Allocator) void {
        self.items.deinit(alloc);
    }

    pub fn withRoot(alloc: std.mem.Allocator, root: View) !Stack {
        var s: Stack = .init();
        try s.items.append(alloc, root);
        return s;
    }

    pub fn current(self: *const Stack) *const View {
        return &self.items.items[self.items.items.len - 1];
    }

    pub fn currentMut(self: *Stack) *View {
        return &self.items.items[self.items.items.len - 1];
    }

    pub fn depth(self: *const Stack) usize {
        return self.items.items.len;
    }

    pub fn push(self: *Stack, alloc: std.mem.Allocator, view: View) !void {
        try self.items.append(alloc, view);
    }

    pub fn pop(self: *Stack) bool {
        if (self.items.items.len <= 1) return false;
        _ = self.items.pop();
        return true;
    }

    pub fn popToRoot(self: *Stack) void {
        while (self.items.items.len > 1) _ = self.items.pop();
    }

    pub fn replaceTop(self: *Stack, view: View) void {
        if (self.items.items.len == 0) return;
        self.items.items[self.items.items.len - 1] = view;
    }

    pub fn breadcrumb(self: *const Stack, writer: *std.Io.Writer) !void {
        for (self.items.items, 0..) |v, i| {
            if (i > 0) try writer.writeAll(" › ");
            try writer.writeAll(v.title);
        }
    }
};

test "stack push/pop/root" {
    const alloc = std.testing.allocator;
    var s = try Stack.withRoot(alloc, .{ .kind = .main, .title = "main" });
    defer s.deinit(alloc);

    try std.testing.expectEqual(@as(usize, 1), s.depth());
    try std.testing.expectEqual(ViewKind.main, s.current().kind);

    try s.push(alloc, .{ .kind = .settings, .title = "settings" });
    try s.push(alloc, .{ .kind = .message_detail, .title = "msg" });
    try std.testing.expectEqual(@as(usize, 3), s.depth());

    try std.testing.expect(s.pop());
    try std.testing.expectEqual(ViewKind.settings, s.current().kind);

    s.popToRoot();
    try std.testing.expectEqual(@as(usize, 1), s.depth());

    try std.testing.expect(!s.pop());
}
