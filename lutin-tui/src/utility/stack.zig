const std = @import("std");

pub fn Stack(comptime Type: type) type {
    return struct {
        items: std.ArrayList(Type),

        pub fn init() Stack {
            return .{ .items = .empty };
        }

        pub fn deinit(self: *Stack, alloc: std.mem.Allocator) void {
            self.items.deinit(alloc);
        }

        pub fn with_root(alloc: std.mem.Allocator, root: Type) !Stack {
            var s: Stack = .init();
            try s.items.append(alloc, root);
            return s;
        }

        pub fn current(self: *const Stack) *const Type {
            return &self.items.items[self.items.items.len - 1];
        }

        pub fn current_mut(self: *Stack) *Type {
            return &self.items.items[self.items.items.len - 1];
        }

        pub fn depth(self: *const Stack) usize {
            return self.items.items.len;
        }

        pub fn push(self: *Stack, alloc: std.mem.Allocator, view: Type) !void {
            try self.items.append(alloc, view);
        }

        pub fn pop(self: *Stack) bool {
            if (self.items.items.len <= 1) return false;
            _ = self.items.pop();
            return true;
        }

        pub fn pop_to_root(self: *Stack) void {
            while (self.items.items.len > 1) _ = self.items.pop();
        }

        pub fn replace_top(self: *Stack, item: Type) void {
            if (self.items.items.len == 0) return;
            self.items.items[self.items.items.len - 1] = item;
        }

        pub fn breadcrumb(self: *const Stack, writer: *std.Io.Writer) !void {
            for (self.items.items, 0..) |v, i| {
                if (i > 0) try writer.writeAll(" › ");
                try writer.writeAll(v.title);
            }
        }
    };
}
