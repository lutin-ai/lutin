const std = @import("std");
const mode_mod = @import("mode.zig");
const tab_mod = @import("tab.zig");
const nav = @import("nav.zig");
const input_widget = @import("../ui/input.zig");

pub const Mode = mode_mod.Mode;
pub const Leader = mode_mod.Leader;
pub const Tab = tab_mod.Tab;

pub const Overlay = union(enum) {
    none,
    which_key: WhichKey,
    picker: Picker,
    approval: Approval,
    confirm: Confirm,

    pub const WhichKey = struct {
        leader: Leader,
    };
    pub const Picker = struct {
        kind: Kind,
        query: input_widget.TextInput,
        pub const Kind = enum { project, session, persona, command, file, workflow };
    };
    pub const Approval = struct {
        tool_name: []const u8,
    };
    pub const Confirm = struct {
        prompt: []const u8,
    };
};

pub const Flash = struct {
    text: std.ArrayList(u8) = .empty,
    until_ms: i64 = 0,

    pub fn deinit(self: *Flash, alloc: std.mem.Allocator) void {
        self.text.deinit(alloc);
    }

    pub fn set(self: *Flash, alloc: std.mem.Allocator, msg: []const u8, now_ms: i64, ttl_ms: i64) !void {
        self.text.clearRetainingCapacity();
        try self.text.appendSlice(alloc, msg);
        self.until_ms = now_ms + ttl_ms;
    }

    pub fn active(self: *const Flash, now_ms: i64) bool {
        return self.text.items.len > 0 and now_ms < self.until_ms;
    }
};

pub const Model = struct {
    alloc: std.mem.Allocator,

    mode: Mode = .normal,
    leader: Leader = .none,
    leader_since_ms: i64 = 0,

    tabs: std.ArrayList(Tab) = .empty,
    active_tab: usize = 0,

    composer: input_widget.TextInput,
    command_line: input_widget.TextInput,

    overlay: Overlay = .none,
    flash: Flash = .{},

    quit: bool = false,
    dirty: bool = true,

    pub fn init(alloc: std.mem.Allocator) !Model {
        var m: Model = .{
            .alloc = alloc,
            .composer = .init(),
            .command_line = .init(),
        };
        const first = try Tab.init(alloc, 1, "scratch");
        try m.tabs.append(alloc, first);
        return m;
    }

    pub fn deinit(self: *Model) void {
        for (self.tabs.items) |*t| t.deinit(self.alloc);
        self.tabs.deinit(self.alloc);
        self.composer.deinit(self.alloc);
        self.command_line.deinit(self.alloc);
        self.flash.deinit(self.alloc);
    }

    pub fn activeTab(self: anytype) switch (@TypeOf(self)) {
        *Model => *Tab,
        *const Model => *const Tab,
        else => @compileError("activeTab expects *Model or *const Model"),
    } {
        return &self.tabs.items[self.active_tab];
    }

    pub fn stack(self: *Model) *nav.Stack {
        return &self.activeTab().stack;
    }

    pub fn pushView(self: *Model, view: nav.View) !void {
        try self.stack().push(self.alloc, view);
        self.dirty = true;
    }

    pub fn popView(self: *Model) bool {
        const ok = self.stack().pop();
        if (ok) self.dirty = true;
        return ok;
    }

    pub fn popToRoot(self: *Model) void {
        self.stack().popToRoot();
        self.dirty = true;
    }
};
