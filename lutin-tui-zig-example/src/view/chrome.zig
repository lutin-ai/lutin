const std = @import("std");
const vaxis = @import("vaxis");
const Window = vaxis.Window;

const model_mod = @import("../model/model.zig");
const theme = @import("../ui/theme.zig");
const text = @import("../ui/text.zig");
const keyhint = @import("../ui/keyhint.zig");

const Model = model_mod.Model;

pub fn renderBreadcrumb(win: Window, m: *const Model, th: theme.Theme) void {
    text.fillRow(win, 0, th.style(.chrome));
    var buf: [256]u8 = undefined;
    var sw = std.Io.Writer.fixed(&buf);
    m.tabs.items[m.active_tab].stack.breadcrumb(&sw) catch {};
    const crumb = sw.buffered();
    text.writeAt(win, 1, 0, crumb, th.style(.chrome));

    const right = "lutin";
    if (text.visualWidth(right) + 2 < win.width) {
        const col = win.width - text.visualWidth(right) - 1;
        text.writeAt(win, col, 0, right, th.style(.chrome_dim));
    }
}

pub fn renderTabs(win: Window, m: *const Model, th: theme.Theme) void {
    text.fillRow(win, 0, th.style(.chrome));
    var col: u16 = 0;
    for (m.tabs.items, 0..) |t, i| {
        var num_buf: [4]u8 = undefined;
        const num = std.fmt.bufPrint(&num_buf, "{d}", .{i + 1}) catch "?";
        const active = i == m.active_tab;
        const style = th.style(if (active) .accent else .chrome_dim);
        col += writeSeg(win, col, 0, " ", style);
        col += writeSeg(win, col, 0, num, style);
        col += writeSeg(win, col, 0, ":", style);
        col += writeSeg(win, col, 0, t.title.items, style);
        if (t.indicator != .none) {
            const dot = switch (t.indicator) {
                .running => "●",
                .approval => "!",
                .completed => "*",
                .none => " ",
            };
            col += writeSeg(win, col, 0, dot, th.style(.running));
        }
        col += writeSeg(win, col, 0, " ", style);
        if (col >= win.width) break;
    }
}

pub fn renderStatus(win: Window, m: *const Model, now_ms: i64, th: theme.Theme) void {
    text.fillRow(win, 0, th.style(.chrome));

    const mode_style = switch (m.mode) {
        .normal => th.style(.mode_normal),
        .insert => th.style(.mode_insert),
        .command => th.style(.mode_command),
    };
    var col: u16 = 0;
    col += writeSeg(win, col, 0, m.mode.label(), mode_style);
    col += writeSeg(win, col, 0, " ", th.style(.chrome));

    const focus_label = switch (m.activeTab().pane) {
        .chat => "chat",
        .rail => "rail",
        .composer => "composer",
    };
    col += writeSeg(win, col, 0, focus_label, th.style(.chrome_dim));

    if (m.flash.active(now_ms)) {
        col += writeSeg(win, col, 0, "  ", th.style(.chrome));
        col += writeSeg(win, col, 0, m.flash.text.items, th.style(.flash));
    }

    const hints = defaultHints(m);
    if (hints.len > 0 and col < win.width - 4) {
        const right_win = win.child(.{ .x_off = @intCast(col + 2) });
        keyhint.renderRow(right_win, 0, hints, th);
    }
}

fn defaultHints(m: *const Model) []const keyhint.Hint {
    return switch (m.mode) {
        .normal => switch (m.leader) {
            .none => &normal_hints,
            .space => &space_hints,
            .goto => &goto_hints,
            .space_t => &space_t_hints,
            .z => &z_hints,
        },
        .insert => &insert_hints,
        .command => &command_hints,
    };
}

const normal_hints = [_]keyhint.Hint{
    .{ .key = "i", .label = "insert" },
    .{ .key = ":", .label = "cmd" },
    .{ .key = "␣", .label = "leader" },
    .{ .key = "g", .label = "goto" },
    .{ .key = "q", .label = "quit" },
};
const space_hints = [_]keyhint.Hint{
    .{ .key = "p", .label = "project" },
    .{ .key = "s", .label = "session" },
    .{ .key = "k", .label = "persona" },
    .{ .key = ",", .label = "settings" },
    .{ .key = "?", .label = "help" },
    .{ .key = "t", .label = "tab…" },
};
const goto_hints = [_]keyhint.Hint{
    .{ .key = "a", .label = "back" },
    .{ .key = "A", .label = "root" },
    .{ .key = "d", .label = "dashboard" },
    .{ .key = "g", .label = "top" },
};
const space_t_hints = [_]keyhint.Hint{
    .{ .key = "n", .label = "new" },
    .{ .key = "c", .label = "close" },
    .{ .key = "r", .label = "rename" },
};
const z_hints = [_]keyhint.Hint{
    .{ .key = "c", .label = "collapse" },
    .{ .key = "o", .label = "expand" },
    .{ .key = "a", .label = "toggle" },
};
const insert_hints = [_]keyhint.Hint{
    .{ .key = "↵", .label = "send" },
    .{ .key = "⇧↵", .label = "newline" },
    .{ .key = "esc", .label = "normal" },
};
const command_hints = [_]keyhint.Hint{
    .{ .key = "↵", .label = "run" },
    .{ .key = "esc", .label = "cancel" },
};

fn writeSeg(win: Window, col: u16, row: u16, t: []const u8, style: vaxis.Style) u16 {
    const res = win.printSegment(.{ .text = t, .style = style }, .{
        .col_offset = col,
        .row_offset = row,
        .wrap = .none,
    });
    return res.col - col;
}
