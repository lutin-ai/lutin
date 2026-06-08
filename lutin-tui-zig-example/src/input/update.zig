const std = @import("std");
const vaxis = @import("vaxis");
const Key = vaxis.Key;

const model_mod = @import("../model/model.zig");
const nav = @import("../model/nav.zig");
const keymap = @import("keymap.zig");
const action_mod = @import("action.zig");
const app = @import("../app.zig");
const types = @import("../llm/types.zig");

const Model = model_mod.Model;
const Mode = model_mod.Mode;
const Leader = model_mod.Leader;
const Action = action_mod.Action;

pub fn onKey(m: *Model, key: Key, now_ms: i64, ctx: ?*app.Ctx) !void {
    if (m.overlay != .none) {
        try onKeyOverlay(m, key);
        return;
    }
    switch (m.mode) {
        .normal => try onKeyNormal(m, key, now_ms, ctx),
        .insert => try onKeyInsert(m, key, now_ms, ctx),
        .command => try onKeyCommand(m, key),
    }
}

fn onKeyNormal(m: *Model, key: Key, now_ms: i64, ctx: ?*app.Ctx) !void {
    const act = keymap.resolveNormal(m.leader, key);
    try apply(m, act, now_ms, ctx);
}

fn onKeyInsert(m: *Model, key: Key, now_ms: i64, ctx: ?*app.Ctx) !void {
    if (key.codepoint == Key.escape) {
        m.mode = .normal;
        m.dirty = true;
        return;
    }
    if (key.codepoint == Key.enter) {
        try apply(m, .submit_composer, now_ms, ctx);
        return;
    }
    if (key.codepoint == Key.backspace) {
        m.composer.backspace();
        m.dirty = true;
        return;
    }
    if (key.text) |text| {
        try m.composer.insert(m.alloc, text);
        m.dirty = true;
    }
}

fn onKeyCommand(m: *Model, key: Key) !void {
    if (key.codepoint == Key.escape) {
        m.command_line.clear(m.alloc);
        m.mode = .normal;
        m.dirty = true;
        return;
    }
    if (key.codepoint == Key.enter) {
        try apply(m, .submit_command, 0, null);
        return;
    }
    if (key.codepoint == Key.backspace) {
        if (m.command_line.buf.items.len == 0) {
            m.mode = .normal;
        } else {
            m.command_line.backspace();
        }
        m.dirty = true;
        return;
    }
    if (key.text) |text| {
        try m.command_line.insert(m.alloc, text);
        m.dirty = true;
    }
}

fn onKeyOverlay(m: *Model, key: Key) !void {
    if (key.codepoint == Key.escape) {
        m.overlay = .none;
        m.dirty = true;
    }
}

fn apply(m: *Model, act: Action, now_ms: i64, ctx: ?*app.Ctx) !void {
    switch (act) {
        .none => {},
        .quit => m.quit = true,

        .enter_insert_mode => {
            m.mode = .insert;
            m.leader = .none;
            m.dirty = true;
        },
        .enter_command_mode => {
            m.mode = .command;
            m.leader = .none;
            m.command_line.clear(m.alloc);
            m.dirty = true;
        },
        .leave_to_normal => {
            m.mode = .normal;
            m.dirty = true;
        },

        .begin_leader_space => setLeader(m, .space, now_ms),
        .begin_leader_goto => setLeader(m, .goto, now_ms),
        .begin_leader_space_t => setLeader(m, .space_t, now_ms),
        .begin_leader_z => setLeader(m, .z, now_ms),
        .clear_leader => {
            m.leader = .none;
            m.dirty = true;
        },

        .move_selection_down => moveSel(m, 1),
        .move_selection_up => moveSel(m, -1),
        .move_selection_top => topSel(m),
        .move_selection_bottom => bottomSel(m),

        .activate_focused => try activate(m),
        .cycle_subfocus => {},

        .focus_pane_left, .focus_pane_right, .focus_pane_up, .focus_pane_down => togglePane(m),

        .nav_back => {
            _ = m.popView();
            m.leader = .none;
        },
        .nav_back_root => {
            m.popToRoot();
            m.leader = .none;
        },
        .open_dashboard => try pushIfNew(m, .{ .kind = .dashboard, .title = "dashboard" }),
        .open_settings => try pushIfNew(m, .{ .kind = .settings, .title = "settings" }),
        .open_help => try pushIfNew(m, .{ .kind = .help, .title = "help" }),

        .open_project_picker,
        .open_session_picker,
        .open_persona_picker,
        .open_command_palette,
        .open_file_picker,
        .open_workflow_picker,
        => {
            m.overlay = .{ .picker = .{
                .kind = pickerKind(act),
                .query = .init(),
            } };
            m.leader = .none;
            m.dirty = true;
        },

        .new_tab => {
            const id: u32 = @intCast(m.tabs.items.len + 1);
            const t = try @import("../model/tab.zig").Tab.init(m.alloc, id, "scratch");
            try m.tabs.append(m.alloc, t);
            m.active_tab = m.tabs.items.len - 1;
            m.leader = .none;
            m.dirty = true;
        },
        .close_tab => {
            if (m.tabs.items.len > 1) {
                var t = m.tabs.orderedRemove(m.active_tab);
                t.deinit(m.alloc);
                if (m.active_tab >= m.tabs.items.len) m.active_tab = m.tabs.items.len - 1;
            }
            m.leader = .none;
            m.dirty = true;
        },
        .rename_tab => {
            m.leader = .none;
        },
        .next_tab => {
            m.active_tab = (m.active_tab + 1) % m.tabs.items.len;
            m.leader = .none;
            m.dirty = true;
        },
        .prev_tab => {
            m.active_tab = if (m.active_tab == 0) m.tabs.items.len - 1 else m.active_tab - 1;
            m.leader = .none;
            m.dirty = true;
        },
        .goto_tab => |n| {
            const idx: usize = if (n == 0) 0 else n - 1;
            if (idx < m.tabs.items.len) m.active_tab = idx;
            m.dirty = true;
        },

        .toggle_rail => {
            m.activeTab().rail_visible = !m.activeTab().rail_visible;
            m.leader = .none;
            m.dirty = true;
        },
        .show_pending_approvals, .toggle_stt, .read_aloud => {
            m.leader = .none;
        },

        .submit_composer => try submitComposer(m, now_ms, ctx),
        .submit_command => try runCommand(m),
    }
}

fn setLeader(m: *Model, leader: Leader, now_ms: i64) void {
    m.leader = leader;
    m.leader_since_ms = now_ms;
    m.dirty = true;
}

fn moveSel(m: *Model, d: isize) void {
    const t = m.activeTab();
    switch (t.pane) {
        .chat => t.chat_list.move(d, 0),
        .rail => t.rail_list.move(d, 5),
        .composer => {},
    }
    m.dirty = true;
}

fn topSel(m: *Model) void {
    const t = m.activeTab();
    switch (t.pane) {
        .chat => t.chat_list.top(),
        .rail => t.rail_list.top(),
        .composer => {},
    }
    m.leader = .none;
    m.dirty = true;
}

fn bottomSel(m: *Model) void {
    const t = m.activeTab();
    switch (t.pane) {
        .chat => t.chat_list.bottom(0),
        .rail => t.rail_list.bottom(5),
        .composer => {},
    }
    m.dirty = true;
}

fn togglePane(m: *Model) void {
    const t = m.activeTab();
    t.pane = if (t.pane == .chat) .rail else .chat;
    m.dirty = true;
}

fn activate(m: *Model) !void {
    const t = m.activeTab();
    switch (t.pane) {
        .chat => try pushIfNew(m, .{ .kind = .message_detail, .title = "message" }),
        .rail => {
            const card_views = [_]nav.View{
                .{ .kind = .settings, .title = "persona" },
                .{ .kind = .subagent_detail, .title = "sub-agents" },
                .{ .kind = .settings, .title = "session" },
                .{ .kind = .dashboard, .title = "project" },
                .{ .kind = .settings, .title = "metrics" },
            };
            const idx = @min(t.rail_list.selected, card_views.len - 1);
            try pushIfNew(m, card_views[idx]);
        },
        .composer => {},
    }
}

fn pushIfNew(m: *Model, view: nav.View) !void {
    if (m.stack().current().kind != view.kind) try m.pushView(view);
    m.leader = .none;
}

fn pickerKind(act: Action) model_mod.Overlay.Picker.Kind {
    return switch (act) {
        .open_project_picker => .project,
        .open_session_picker => .session,
        .open_persona_picker => .persona,
        .open_command_palette => .command,
        .open_file_picker => .file,
        .open_workflow_picker => .workflow,
        else => .command,
    };
}

fn submitComposer(m: *Model, now_ms: i64, ctx: ?*app.Ctx) !void {
    const text = m.composer.slice();
    if (text.len == 0) {
        m.dirty = true;
        return;
    }
    const tab = m.activeTab();
    const owned = try m.alloc.dupe(u8, text);
    try tab.messages.append(m.alloc, .{ .user = owned });
    m.composer.clear(m.alloc);
    m.mode = .normal;
    tab.indicator = .running;

    if (ctx) |c| {
        c.spawnRunner(tab.id, tab.messages.items) catch |err| {
            const msg = try std.fmt.allocPrint(m.alloc, "spawn failed: {s}", .{@errorName(err)});
            defer m.alloc.free(msg);
            try m.flash.set(m.alloc, msg, now_ms, 5_000);
        };
        try m.flash.set(m.alloc, "sending…", now_ms, 2_000);
    } else {
        try m.flash.set(m.alloc, "no agent ctx (offline)", now_ms, 2_000);
    }
    m.dirty = true;
}

fn runCommand(m: *Model) !void {
    const cmd = m.command_line.slice();
    if (std.mem.eql(u8, cmd, "q") or std.mem.eql(u8, cmd, "quit")) {
        m.quit = true;
    } else if (std.mem.eql(u8, cmd, "settings")) {
        try m.pushView(.{ .kind = .settings, .title = "settings" });
    } else if (std.mem.eql(u8, cmd, "dashboard")) {
        try m.pushView(.{ .kind = .dashboard, .title = "dashboard" });
    } else if (std.mem.eql(u8, cmd, "help")) {
        try m.pushView(.{ .kind = .help, .title = "help" });
    }
    m.command_line.clear(m.alloc);
    m.mode = .normal;
    m.dirty = true;
}
