const std = @import("std");
const vaxis = @import("vaxis");
const Window = vaxis.Window;
const theme = @import("theme.zig");
const text = @import("text.zig");

pub const ListState = struct {
    selected: usize = 0,
    offset: usize = 0,

    pub fn move(self: *ListState, delta: isize, len: usize) void {
        if (len == 0) {
            self.selected = 0;
            return;
        }
        const cur: isize = @intCast(self.selected);
        var n = cur + delta;
        if (n < 0) n = 0;
        if (n >= @as(isize, @intCast(len))) n = @intCast(len - 1);
        self.selected = @intCast(n);
    }

    pub fn top(self: *ListState) void {
        self.selected = 0;
    }

    pub fn bottom(self: *ListState, len: usize) void {
        if (len == 0) self.selected = 0 else self.selected = len - 1;
    }

    fn ensureVisible(self: *ListState, viewport: usize) void {
        if (viewport == 0) return;
        if (self.selected < self.offset) self.offset = self.selected;
        if (self.selected >= self.offset + viewport) self.offset = self.selected + 1 - viewport;
    }
};

pub fn RenderItemFn(comptime T: type) type {
    return *const fn (win: Window, row: u16, item: T, selected: bool, th: theme.Theme) void;
}

pub fn render(
    comptime T: type,
    win: Window,
    items: []const T,
    state: *ListState,
    th: theme.Theme,
    renderItem: RenderItemFn(T),
) void {
    state.ensureVisible(win.height);
    var row: u16 = 0;
    while (row < win.height) : (row += 1) {
        const idx = state.offset + row;
        if (idx >= items.len) break;
        renderItem(win, row, items[idx], idx == state.selected, th);
    }
}

pub fn defaultRowText(win: Window, row: u16, label: []const u8, selected: bool, th: theme.Theme) void {
    const style = th.style(if (selected) .selected else .text);
    text.fillRow(win, row, style);
    text.writeAt(win, 1, row, label, style);
}
