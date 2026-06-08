const std = @import("std");
const vaxis = @import("vaxis");
const Window = vaxis.Window;
const theme = @import("theme.zig");
const text = @import("text.zig");

pub const TextInput = struct {
    buf: std.ArrayList(u8),
    cursor: usize = 0,

    pub fn init() TextInput {
        return .{ .buf = .empty, .cursor = 0 };
    }

    pub fn deinit(self: *TextInput, alloc: std.mem.Allocator) void {
        self.buf.deinit(alloc);
    }

    pub fn clear(self: *TextInput, alloc: std.mem.Allocator) void {
        self.buf.clearAndFree(alloc);
        self.cursor = 0;
    }

    pub fn slice(self: *const TextInput) []const u8 {
        return self.buf.items;
    }

    pub fn insert(self: *TextInput, alloc: std.mem.Allocator, bytes: []const u8) !void {
        try self.buf.insertSlice(alloc, self.cursor, bytes);
        self.cursor += bytes.len;
    }

    pub fn backspace(self: *TextInput) void {
        if (self.cursor == 0 or self.buf.items.len == 0) return;
        _ = self.buf.orderedRemove(self.cursor - 1);
        self.cursor -= 1;
    }

    pub fn deleteWordBack(self: *TextInput) void {
        if (self.cursor == 0) return;
        var i = self.cursor;
        while (i > 0 and self.buf.items[i - 1] == ' ') : (i -= 1) {}
        while (i > 0 and self.buf.items[i - 1] != ' ') : (i -= 1) {}
        self.buf.replaceRangeAssumeCapacity(i, self.cursor - i, "");
        self.cursor = i;
    }

    pub fn moveLeft(self: *TextInput) void {
        if (self.cursor > 0) self.cursor -= 1;
    }
    pub fn moveRight(self: *TextInput) void {
        if (self.cursor < self.buf.items.len) self.cursor += 1;
    }

    pub fn render(
        self: *const TextInput,
        win: Window,
        prompt: []const u8,
        th: theme.Theme,
        focused: bool,
    ) void {
        const base = th.style(.chrome);
        text.fillRow(win, 0, base);
        var col: u16 = 0;
        if (prompt.len > 0) {
            text.writeAt(win, col, 0, prompt, th.style(.accent));
            col += text.visualWidth(prompt);
        }
        text.writeAt(win, col, 0, self.buf.items, base);
        if (focused) {
            const cursor_col = col + @as(u16, @intCast(self.cursor));
            win.showCursor(cursor_col, 0);
        }
    }
};
