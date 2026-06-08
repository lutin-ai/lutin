const std = @import("std");
const vaxis = @import("vaxis");
const Window = vaxis.Window;
const Segment = vaxis.Segment;
const theme = @import("theme.zig");

pub const Hint = struct {
    key: []const u8,
    label: []const u8,
};

pub fn renderRow(win: Window, row: u16, hints: []const Hint, th: theme.Theme) void {
    const key_style = th.style(.accent);
    const label_style = th.style(.chrome_dim);
    const sep_style = th.style(.chrome_dim);
    var col: u16 = 0;
    for (hints, 0..) |h, i| {
        if (i > 0) {
            col += writeSeg(win, col, row, "  ", sep_style);
        }
        col += writeSeg(win, col, row, h.key, key_style);
        col += writeSeg(win, col, row, " ", label_style);
        col += writeSeg(win, col, row, h.label, label_style);
        if (col >= win.width) break;
    }
}

fn writeSeg(win: Window, col: u16, row: u16, text: []const u8, style: vaxis.Style) u16 {
    const res = win.printSegment(.{ .text = text, .style = style }, .{
        .col_offset = col,
        .row_offset = row,
        .wrap = .none,
    });
    return res.col - col;
}
