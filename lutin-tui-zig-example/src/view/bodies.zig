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
        const cards = [_][]const u8{ "Persona", "Sub-agents", "Session", "Project", "Metrics" };
        for (cards, 0..) |label, i| {
            const sel = i == tab.rail_list.selected and tab.pane == .rail;
            const row: u16 = @intCast(i * 2);
            if (row + 1 >= rail_body.height) break;
            const style = th.style(if (sel) .selected else .text);
            text.fillRow(rail_body, row, style);
            text.writeAt(rail_body, 1, row, label, style);
        }
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
