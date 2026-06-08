const vaxis = @import("vaxis");
const Window = vaxis.Window;
const Segment = vaxis.Segment;
const Style = vaxis.Style;

pub const Align = enum { left, center, right };

pub fn writeAt(win: Window, col: u16, row: u16, text: []const u8, style: Style) void {
    _ = win.printSegment(.{ .text = text, .style = style }, .{
        .col_offset = col,
        .row_offset = row,
        .wrap = .none,
    });
}

pub fn line(win: Window, row: u16, text: []const u8, style: Style, alignment: Align) void {
    const w = visualWidth(text);
    const col: u16 = switch (alignment) {
        .left => 0,
        .center => if (w >= win.width) 0 else (win.width - w) / 2,
        .right => if (w >= win.width) 0 else win.width - w,
    };
    writeAt(win, col, row, text, style);
}

pub fn fillRow(win: Window, row: u16, style: Style) void {
    if (row >= win.height) return;
    var col: u16 = 0;
    while (col < win.width) : (col += 1) {
        win.writeCell(col, row, .{ .style = style });
    }
}

pub fn visualWidth(text: []const u8) u16 {
    var n: u16 = 0;
    for (text) |b| {
        if (b & 0xc0 != 0x80) n += 1;
    }
    return n;
}
