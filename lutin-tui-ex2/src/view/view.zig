const vaxis = @import("vaxis");
const Window = vaxis.Window;

const model_mod = @import("../model/model.zig");
const theme = @import("../ui/theme.zig");
const text = @import("../ui/text.zig");
const chrome = @import("chrome.zig");
const bodies = @import("bodies.zig");
const overlays = @import("overlays.zig");

const Model = model_mod.Model;

pub fn render(root: Window, m: *const Model, now_ms: i64, th: theme.Theme) void {
    root.fill(.{ .style = th.style(.text) });

    const W = root.width;
    const H = root.height;
    if (H < 8 or W < 20) return;

    var row: u16 = 0;
    const breadcrumb = root.child(.{ .y_off = row, .width = W, .height = 1 });
    chrome.renderBreadcrumb(breadcrumb, m, th);
    row += 1;

    const tabs = root.child(.{ .y_off = row, .width = W, .height = 1 });
    chrome.renderTabs(tabs, m, th);
    row += 1;

    const composer_h: u16 = if (m.mode == .insert) 4 else 2;
    const command_h: u16 = if (m.mode == .command) 1 else 0;
    const status_h: u16 = 1;
    const body_h: u16 = H - row - composer_h - command_h - status_h;

    const body = root.child(.{ .y_off = row, .width = W, .height = body_h });
    bodies.render(body, m, th);
    row += body_h;

    const composer = root.child(.{ .y_off = row, .width = W, .height = composer_h });
    renderComposer(composer, m, th);
    row += composer_h;

    if (command_h > 0) {
        const cmd = root.child(.{ .y_off = row, .width = W, .height = 1 });
        renderCommandLine(cmd, m, th);
        row += 1;
    }

    const status = root.child(.{ .y_off = row, .width = W, .height = 1 });
    chrome.renderStatus(status, m, now_ms, th);

    overlays.render(root, m, th);
}

fn renderComposer(win: Window, m: *const Model, th: theme.Theme) void {
    const Pane = @import("../ui/pane.zig").Pane;
    const pane = Pane{
        .title = "composer",
        .focused = m.mode == .insert,
        .th = th,
    };
    const body = pane.body(win);
    m.composer.render(body, "", th, m.mode == .insert);
}

fn renderCommandLine(win: Window, m: *const Model, th: theme.Theme) void {
    text.fillRow(win, 0, th.style(.chrome));
    text.writeAt(win, 0, 0, ":", th.style(.accent));
    text.writeAt(win, 1, 0, m.command_line.slice(), th.style(.chrome));
    win.showCursor(@intCast(1 + m.command_line.cursor), 0);
}
