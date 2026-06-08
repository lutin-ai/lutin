const vaxis = @import("vaxis");
const Key = vaxis.Key;
const action_mod = @import("action.zig");
const mode_mod = @import("../model/mode.zig");

pub const Action = action_mod.Action;
pub const Mode = mode_mod.Mode;
pub const Leader = mode_mod.Leader;

pub fn resolveNormal(leader: Leader, key: Key) Action {
    return switch (leader) {
        .none => normalRoot(key),
        .space => leaderSpace(key),
        .goto => leaderGoto(key),
        .space_t => leaderSpaceT(key),
        .z => leaderZ(key),
    };
}

fn matchPlain(key: Key, cp: u21) bool {
    return key.codepoint == cp and !key.mods.ctrl and !key.mods.alt and !key.mods.super;
}

fn normalRoot(key: Key) Action {
    if (matchPlain(key, 'q')) return .quit;
    if (matchPlain(key, ':')) return .enter_command_mode;
    if (matchPlain(key, 'i')) return .enter_insert_mode;
    if (matchPlain(key, ' ')) return .begin_leader_space;
    if (matchPlain(key, 'z')) return .begin_leader_z;
    if (matchPlain(key, 'g')) return .begin_leader_goto;
    if (matchPlain(key, 'G')) return .move_selection_bottom;
    if (matchPlain(key, 'j')) return .move_selection_down;
    if (matchPlain(key, 'k')) return .move_selection_up;
    if (key.codepoint == Key.enter) return .activate_focused;
    if (key.codepoint == Key.escape) return .clear_leader;
    if (key.codepoint == Key.tab) return .cycle_subfocus;
    if (key.mods.alt) {
        if (key.codepoint == 'h') return .focus_pane_left;
        if (key.codepoint == 'l') return .focus_pane_right;
        if (key.codepoint == 'j') return .focus_pane_down;
        if (key.codepoint == 'k') return .focus_pane_up;
    }
    if (key.codepoint >= '1' and key.codepoint <= '9' and !key.mods.ctrl and !key.mods.alt) {
        return .{ .goto_tab = @intCast(key.codepoint - '0') };
    }
    return .none;
}

fn leaderSpace(key: Key) Action {
    if (matchPlain(key, 'p')) return .open_project_picker;
    if (matchPlain(key, 's')) return .open_session_picker;
    if (matchPlain(key, 'k')) return .open_persona_picker;
    if (matchPlain(key, 'f')) return .open_file_picker;
    if (matchPlain(key, 'w')) return .open_workflow_picker;
    if (matchPlain(key, ',')) return .open_settings;
    if (matchPlain(key, '?')) return .open_help;
    if (matchPlain(key, ':')) return .open_command_palette;
    if (matchPlain(key, 'r')) return .toggle_rail;
    if (matchPlain(key, 'a')) return .show_pending_approvals;
    if (matchPlain(key, 'v')) return .toggle_stt;
    if (matchPlain(key, 'V')) return .read_aloud;
    if (matchPlain(key, 't')) return .begin_leader_space_t;
    if (matchPlain(key, '[')) return .prev_tab;
    if (matchPlain(key, ']')) return .next_tab;
    if (key.codepoint == Key.escape) return .clear_leader;
    return .clear_leader;
}

fn leaderGoto(key: Key) Action {
    if (matchPlain(key, 'a')) return .nav_back;
    if (matchPlain(key, 'A')) return .nav_back_root;
    if (matchPlain(key, 'h')) return .nav_back_root;
    if (matchPlain(key, 'd')) return .open_dashboard;
    if (matchPlain(key, 'g')) return .move_selection_top;
    return .clear_leader;
}

fn leaderSpaceT(key: Key) Action {
    if (matchPlain(key, 'n')) return .new_tab;
    if (matchPlain(key, 'c')) return .close_tab;
    if (matchPlain(key, 'r')) return .rename_tab;
    return .clear_leader;
}

fn leaderZ(key: Key) Action {
    if (matchPlain(key, 's')) return .split_toggle;
    if (matchPlain(key, 'o')) return .split_cycle_orientation;
    // Resize: constrain the side (lhs = left/top)
    if (matchPlain(key, 'h')) return .split_resize_left;
    if (matchPlain(key, 'l')) return .split_resize_right;
    if (matchPlain(key, 'w')) return .split_resize_left;
    if (matchPlain(key, 'e')) return .split_resize_right;
    return .clear_leader;
}
