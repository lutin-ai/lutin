const vaxis = @import("vaxis");
const Style = vaxis.Style;
const Color = vaxis.Color;

pub const Theme = struct {
    bg: Color,
    fg: Color,
    dim: Color,
    accent: Color,
    user: Color,
    assistant: Color,
    success: Color,
    error_: Color,
    running: Color,
    border: Color,
    border_focused: Color,
    chrome_bg: Color,
    flash: Color,

    pub fn style(self: Theme, role: Role) Style {
        return switch (role) {
            .text => .{ .fg = self.fg, .bg = self.bg },
            .dim => .{ .fg = self.dim, .bg = self.bg },
            .accent => .{ .fg = self.accent, .bg = self.bg, .bold = true },
            .selected => .{ .fg = self.bg, .bg = self.accent },
            .border => .{ .fg = self.border },
            .border_focused => .{ .fg = self.border_focused, .bold = true },
            .chrome => .{ .fg = self.fg, .bg = self.chrome_bg },
            .chrome_dim => .{ .fg = self.dim, .bg = self.chrome_bg },
            .mode_normal => .{ .fg = self.bg, .bg = self.accent, .bold = true },
            .mode_insert => .{ .fg = self.bg, .bg = self.success, .bold = true },
            .mode_command => .{ .fg = self.bg, .bg = self.user, .bold = true },
            .flash => .{ .fg = self.flash, .bg = self.chrome_bg, .italic = true },
            .success => .{ .fg = self.success, .bg = self.bg },
            .error_ => .{ .fg = self.error_, .bg = self.bg },
            .running => .{ .fg = self.running, .bg = self.bg },
        };
    }
};

pub const Role = enum {
    text,
    dim,
    accent,
    selected,
    border,
    border_focused,
    chrome,
    chrome_dim,
    mode_normal,
    mode_insert,
    mode_command,
    flash,
    success,
    error_,
    running,
};

pub const slate: Theme = .{
    .bg = .{ .rgb = .{ 0x14, 0x17, 0x1c } },
    .fg = .{ .rgb = .{ 0xcd, 0xd6, 0xe3 } },
    .dim = .{ .rgb = .{ 0x6c, 0x73, 0x82 } },
    .accent = .{ .rgb = .{ 0x82, 0xaa, 0xff } },
    .user = .{ .rgb = .{ 0xc6, 0x9c, 0xf2 } },
    .assistant = .{ .rgb = .{ 0x7e, 0xc4, 0xa4 } },
    .success = .{ .rgb = .{ 0x96, 0xd1, 0x82 } },
    .error_ = .{ .rgb = .{ 0xe6, 0x7e, 0x80 } },
    .running = .{ .rgb = .{ 0xe5, 0xc0, 0x7b } },
    .border = .{ .rgb = .{ 0x33, 0x39, 0x44 } },
    .border_focused = .{ .rgb = .{ 0x82, 0xaa, 0xff } },
    .chrome_bg = .{ .rgb = .{ 0x1c, 0x20, 0x27 } },
    .flash = .{ .rgb = .{ 0xe5, 0xc0, 0x7b } },
};
