pub const Action = union(enum) {
    none,

    quit,
    enter_command_mode,
    enter_insert_mode,
    leave_to_normal,

    begin_leader_space,
    begin_leader_goto,
    begin_leader_space_t,
    begin_leader_z,
    clear_leader,

    move_selection_up,
    move_selection_down,
    move_selection_top,
    move_selection_bottom,
    activate_focused,
    cycle_subfocus,

    focus_pane_left,
    focus_pane_right,
    focus_pane_up,
    focus_pane_down,

    nav_back,
    nav_back_root,
    open_dashboard,
    open_settings,
    open_help,

    open_project_picker,
    open_session_picker,
    open_persona_picker,
    open_command_palette,
    open_file_picker,
    open_workflow_picker,

    new_tab,
    close_tab,
    rename_tab,
    next_tab,
    prev_tab,
    goto_tab: u8,

    toggle_rail,
    show_pending_approvals,
    toggle_stt,
    read_aloud,

    submit_composer,
    submit_command,
};
