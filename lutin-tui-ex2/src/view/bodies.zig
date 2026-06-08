const std = @import("std");
const vaxis = @import("vaxis");
const Window = vaxis.Window;

const model_mod = @import("../model/model.zig");
const nav = @import("../model/nav.zig");
const theme = @import("../ui/theme.zig");
const text = @import("../ui/text.zig");
const Pane = @import("../ui/pane.zig").Pane;
const tab_mod = @import("../model/tab.zig");

const Model = model_mod.Model;
const Tab = tab_mod.Tab;
const SplitMode = tab_mod.SplitMode;
const SplitPosition = tab_mod.SplitPosition;

fn renderChat(win: Window, tab: *const Tab, th: theme.Theme) void {
    if (tab.messages.items.len == 0 and tab.streaming == null) {
        text.writeAt(win, 1, 0, "(no messages yet)", th.style(.dim));
        return;
    }
    var row: u16 = 0;
    var idx: usize = 0;
    for (tab.messages.items) |m| {
        if (row >= win.height) return;
        const selected = idx == tab.chat_list.selected and tab.pane == .chat;
        row = renderMessage(win, row, m, selected, th);
        idx += 1;
    }
    if (tab.streaming) |s| {
        if (row >= win.height) return;
        row = renderStreaming(win, row, s, th);
    }
}

fn renderMessage(win: Window, row_in: u16, m: anytype, selected: bool, th: theme.Theme) u16 {
    var row = row_in;
    const style = th.style(if (selected) .selected else .text);
    switch (m) {
        .user => |s| {
            row = writeRows(win, row, "you: ", s, th.style(.accent), style);
        },
        .assistant => |a| {
            row = writeRows(win, row, "ai:  ", a.text, th.style(.accent), style);
            for (a.tool_calls) |tc| {
                if (row >= win.height) return row;
                text.writeAt(win, 1, row, "  > ", th.style(.dim));
                text.writeAt(win, 5, row, tc.name, th.style(.running));
                row += 1;
            }
        },
        .tool_result => |tr| {
            const prefix: []const u8 = if (tr.is_error) "err: " else "out: ";
            const ps = if (tr.is_error) th.style(.error_) else th.style(.success);
            row = writeRows(win, row, prefix, tr.content, ps, style);
        },
        .system => |s| {
            row = writeRows(win, row, "sys: ", s, th.style(.dim), th.style(.dim));
        },
        .summary => |s| {
            row = writeRows(win, row, "sum: ", s, th.style(.dim), th.style(.dim));
        },
    }
    return row;
}

fn renderStreaming(win: Window, row_in: u16, s: tab_mod.StreamingBuf, th: theme.Theme) u16 {
    var row = row_in;
    row = writeRows(win, row, "ai:  ", s.text.items, th.style(.accent), th.style(.text));
    for (s.tool_calls.items) |c| {
        if (row >= win.height) return row;
        text.writeAt(win, 1, row, "  > ", th.style(.dim));
        text.writeAt(win, 5, row, c.name.items, th.style(.running));
        row += 1;
    }
    return row;
}

// Renders `body` over as many rows as needed to show every '\n'-separated
// line. Returns the next free row. The prefix is drawn only on the first row;
// continuation lines are indented to align with the prefix. printSegment with
// .wrap = .none clips each line to the window width.
fn writeRows(win: Window, row_in: u16, prefix: []const u8, body: []const u8, pstyle: vaxis.Style, bstyle: vaxis.Style) u16 {
    var row = row_in;
    if (row >= win.height) return row;
    text.writeAt(win, 1, row, prefix, pstyle);
    const col: u16 = 1 + text.visualWidth(prefix);

    var it = std.mem.splitScalar(u8, body, '\n');
    while (it.next()) |line| {
        if (row >= win.height) return row;
        text.writeAt(win, col, row, line, bstyle);
        row += 1;
    }
    return row;
}

pub fn render(win: Window, m: *const Model, th: theme.Theme) void {
    const v = m.tabs.items[m.active_tab].stack.current();
    switch (v.kind) {
        .main => renderMain(win, m, th),
        .dashboard => renderDashboard(win, th),
        .message_detail => renderPlaceholder(win, "Message Detail", th),
        .tool_call_detail => renderPlaceholder(win, "Tool Call Detail", th),
        .subagent_detail => renderPlaceholder(win, "Sub-agent Detail", th),
        .settings => renderPlaceholder(win, "Settings", th),
        .help => renderPlaceholder(win, "Help", th),
    }
}

fn renderMain(win: Window, m: *const Model, th: theme.Theme) void {
    const tab = &m.tabs.items[m.active_tab];

    // Determine if we have an active split
    const has_split = tab.split_percent > 0 and tab.rail_visible;

    if (has_split) {
        renderSplitView(win, tab, th);
    } else {
        renderSingleView(win, tab, th);
    }
}

fn renderSingleView(win: Window, tab: *const Tab, th: theme.Theme) void {
    const rail_w: u16 = if (tab.rail_visible) @min(28, win.width / 3) else 0;
    const chat_w: u16 = win.width - rail_w;

    const chat_outer = win.child(.{ .x_off = 0, .y_off = 0, .width = chat_w, .height = win.height });
    const chat = Pane{
        .title = "chat",
        .focused = tab.pane == .chat,
        .th = th,
    };
    const chat_body = chat.body(chat_outer);
    renderChat(chat_body, tab, th);

    if (tab.rail_visible) {
        const rail_outer = win.child(.{ .x_off = @intCast(chat_w), .y_off = 0, .width = rail_w, .height = win.height });
        const rail = Pane{
            .title = "rail",
            .focused = tab.pane == .rail,
            .th = th,
        };
        const rail_body = rail.body(rail_outer);
        renderRailContent(rail_body, tab, th);
    }
}

fn renderSplitView(win: Window, tab: *const Tab, th: theme.Theme) void {
    const pct: u16 = tab.split_percent;

    switch (tab.split_side) {
        .lhs, .rhs => renderVerticalSplit(win, tab, pct, th),
        .top, .bottom => renderHorizontalSplit(win, tab, pct, th),
    }
}

fn renderVerticalSplit(win: Window, tab: *const Tab, pct: u16, th: theme.Theme) void {
    // pct is the percentage of the width for the lhs side (10-90)
    const lhs_w: u16 = @max(10, win.width * pct / 100);
    const rhs_w: u16 = win.width - lhs_w - 1; // -1 for separator

    // LHS = chat
    const chat_left: u16 = switch (tab.split_side) {
        .lhs => 0,
        .rhs => lhs_w + 1,
        .top, .bottom => 0,
    };
    const chat_w = if (tab.split_side == .lhs) lhs_w else rhs_w;

    const chat_outer = win.child(.{ .x_off = chat_left, .y_off = 0, .width = chat_w, .height = win.height });
    const chat = Pane{
        .title = "chat",
        .focused = tab.pane == .chat,
        .th = th,
    };
    const chat_body = chat.body(chat_outer);
    renderChat(chat_body, tab, th);

    // Separator
    const sep_col: u16 = if (tab.split_side == .lhs) lhs_w else 0;
    for (0..win.height) |row| {
        win.writeCell(sep_col, @intCast(row), .{ .style = th.style(.border) });
    }

    // RHS = rail
    const rail_left: u16 = if (tab.split_side == .lhs) lhs_w + 1 else 0;
    const rail_w = if (tab.split_side == .lhs) rhs_w else lhs_w;
    if (rail_w > 0) {
        const rail_outer = win.child(.{ .x_off = rail_left, .y_off = 0, .width = rail_w, .height = win.height });
        const rail = Pane{
            .title = "rail",
            .focused = tab.pane == .rail,
            .th = th,
        };
        const rail_body = rail.body(rail_outer);
        renderRailContent(rail_body, tab, th);
    }
}

fn renderHorizontalSplit(win: Window, tab: *const Tab, pct: u16, th: theme.Theme) void {
    // pct is the percentage of the height for the top side (10-90)
    const top_h: u16 = @max(4, win.height * pct / 100);
    const bot_h: u16 = win.height - top_h - 1; // -1 for separator

    // Top = chat
    const chat_top = switch (tab.split_side) {
        .top => 0,
        .bottom => top_h + 1,
        .lhs, .rhs => 0,
    };
    const chat_h = if (tab.split_side == .top) top_h else bot_h;

    const chat_outer = win.child(.{ .x_off = 0, .y_off = chat_top, .width = win.width, .height = chat_h });
    const chat = Pane{
        .title = "chat",
        .focused = tab.pane == .chat,
        .th = th,
    };
    const chat_body = chat.body(chat_outer);
    renderChat(chat_body, tab, th);

    // Separator
    const sep_row: u16 = if (tab.split_side == .top) top_h else 0;
    var col: u16 = 0;
    while (col < @as(u16, @intCast(win.width))) : (col += 1) {
        win.writeCell(col, sep_row, .{ .style = th.style(.border) });
    }

    // Bottom = rail
    const rail_top: u16 = if (tab.split_side == .top) top_h + 1 else 0;
    const rail_h = if (tab.split_side == .top) bot_h else top_h;
    if (rail_h > 4) {
        const rail_outer = win.child(.{ .x_off = 0, .y_off = rail_top, .width = win.width, .height = rail_h });
        const rail = Pane{
            .title = "rail",
            .focused = tab.pane == .rail,
            .th = th,
        };
        const rail_body = rail.body(rail_outer);
        renderRailContent(rail_body, tab, th);
    }
}

fn renderRailContent(win: Window, tab: *const Tab, th: theme.Theme) void {
    const cards = [_][]const u8{ "Persona", "Sub-agents", "Session", "Project", "Metrics" };
    for (cards, 0..) |label, i| {
        const sel = i == tab.rail_list.selected and tab.pane == .rail;
        const row: u16 = @intCast(i * 2);
        if (row + 1 >= win.height) break;
        const style = th.style(if (sel) .selected else .text);
        text.fillRow(win, row, style);
        text.writeAt(win, 1, row, label, style);
    }
}

fn renderDashboard(win: Window, th: theme.Theme) void {
    text.line(win, 1, "Dashboard", th.style(.accent), .center);
    text.line(win, 3, "Recent sessions go here.", th.style(.dim), .center);
    text.line(win, win.height - 2, "ga: back · gd: dashboard · :q quit", th.style(.dim), .center);
}

fn renderPlaceholder(win: Window, title: []const u8, th: theme.Theme) void {
    text.line(win, 1, title, th.style(.accent), .center);
    text.line(win, 3, "(not yet implemented)", th.style(.dim), .center);
    text.line(win, win.height - 2, "ga: back", th.style(.dim), .center);
}
