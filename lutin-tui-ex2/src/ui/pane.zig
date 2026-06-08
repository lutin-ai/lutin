const vaxis = @import("vaxis");
const Window = vaxis.Window;
const theme = @import("theme.zig");
const text = @import("text.zig");

pub const Pane = struct {
    title: []const u8,
    focused: bool,
    th: theme.Theme,

    pub fn body(self: Pane, parent: Window) Window {
        const border_style = self.th.style(if (self.focused) .border_focused else .border);
        const win = parent.child(.{
            .border = .{ .where = .all, .style = border_style },
        });

        if (self.title.len > 0 and parent.width >= self.title.len + 4) {
            const title_style = self.th.style(if (self.focused) .accent else .dim);
            const open = if (self.focused) "[ " else "  ";
            const close = if (self.focused) " ]" else "  ";
            text.writeAt(parent, 2, 0, open, title_style);
            text.writeAt(parent, 2 + text.visualWidth(open), 0, self.title, title_style);
            text.writeAt(parent, 2 + text.visualWidth(open) + text.visualWidth(self.title), 0, close, title_style);
        }
        return win;
    }
};
