const std = @import("std");
const vaxis = @import("vaxis");
const Window = vaxis.Window;

const model_mod = @import("../model/model.zig");
const theme = @import("../ui/theme.zig");
const text = @import("../ui/text.zig");
const Pane = @import("../ui/pane.zig").Pane;

const Model = model_mod.Model;

pub fn render(win: Window, m: *const Model, th: theme.Theme) void {
    switch (m.overlay) {
        .none => {},
        .which_key => renderWhichKey(win, th),
        .picker => |p| renderPicker(win, p, th),
        .approval => |a| renderApproval(win, a, th),
        .confirm => |c| renderConfirm(win, c, th),
    }
}

fn renderPicker(parent: Window, p: model_mod.Overlay.Picker, th: theme.Theme) void {
    const w: u16 = @min(60, parent.width - 4);
    const h: u16 = @min(18, parent.height - 4);
    const x: u16 = (parent.width - w) / 2;
    const y: u16 = (parent.height - h) / 4;

    const outer = parent.child(.{ .x_off = x, .y_off = y, .width = w, .height = h });
    const pane = Pane{
        .title = switch (p.kind) {
            .project => "project",
            .session => "session",
            .persona => "persona",
            .command => "command palette",
            .file => "file",
            .workflow => "workflow",
        },
        .focused = true,
        .th = th,
    };
    const body = pane.body(outer);

    text.fillRow(body, 0, th.style(.chrome));
    text.writeAt(body, 1, 0, "> ", th.style(.accent));
    text.writeAt(body, 3, 0, p.query.slice(), th.style(.chrome));

    text.writeAt(body, 1, 2, "(no results — wire up data source)", th.style(.dim));
    body.showCursor(@intCast(3 + p.query.cursor), 0);
}

fn renderWhichKey(parent: Window, th: theme.Theme) void {
    const w: u16 = @min(40, parent.width / 3);
    const h: u16 = 6;
    const x: u16 = 0;
    const y: u16 = parent.height - h - 2;
    const outer = parent.child(.{ .x_off = x, .y_off = y, .width = w, .height = h });
    const pane = Pane{ .title = "which-key", .focused = true, .th = th };
    _ = pane.body(outer);
}

fn renderApproval(parent: Window, a: model_mod.Overlay.Approval, th: theme.Theme) void {
    const w: u16 = @min(50, parent.width - 8);
    const h: u16 = 8;
    const x: u16 = (parent.width - w) / 2;
    const y: u16 = (parent.height - h) / 2;
    const outer = parent.child(.{ .x_off = x, .y_off = y, .width = w, .height = h });
    const pane = Pane{ .title = "approval needed", .focused = true, .th = th };
    const body = pane.body(outer);
    text.writeAt(body, 1, 0, a.tool_name, th.style(.accent));
    text.writeAt(body, 1, body.height - 1, "[y] approve   [n] deny   [e] edit   [esc] defer", th.style(.dim));
}

fn renderConfirm(parent: Window, c: model_mod.Overlay.Confirm, th: theme.Theme) void {
    const w: u16 = @min(50, parent.width - 8);
    const h: u16 = 5;
    const x: u16 = (parent.width - w) / 2;
    const y: u16 = (parent.height - h) / 2;
    const outer = parent.child(.{ .x_off = x, .y_off = y, .width = w, .height = h });
    const pane = Pane{ .title = "confirm", .focused = true, .th = th };
    const body = pane.body(outer);
    text.writeAt(body, 1, 0, c.prompt, th.style(.text));
    text.writeAt(body, 1, body.height - 1, "[y/n]", th.style(.dim));
}
