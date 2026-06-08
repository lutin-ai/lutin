const std = @import("std");

pub const Principle = struct {
    title: String,
    description: String,

    /// Always runs if condition is null
    condition: ?String,

    sub_principles: std.ArrayList(Principle),

    triggers: std.ArrayList(Trigger),

    pub const Trigger = enum {
        message,
        tool,
        all,
    };
};


// Triggers: pre message, pre tool, 

applies_to = ["edit", "write", "edit_lines"]
context = ["tool_call", "tool_artifact", "prior_steps"]
max_retries = 2
on_max_retries = "continue"
