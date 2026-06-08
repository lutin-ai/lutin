const std = @import("std");
const app = @import("app.zig");

pub fn main(init: std.process.Init) !void {
    // TODO: --new vs restore-last (no-op until session persistence lands;
    // we always start fresh because there's nothing to restore yet).
    try app.run(init.io, init.gpa, init.environ_map);
}

test {
    _ = @import("model/nav.zig");
    _ = @import("net/http.zig");
    _ = @import("config/paths.zig");
    _ = @import("config/jsonfile.zig");
    _ = @import("config/settings.zig");
    _ = @import("agent/approval.zig");
    _ = @import("agent/loop.zig");
    _ = @import("agent/reducer.zig");
    _ = @import("agent/runner.zig");
    _ = @import("llm/openai_compat.zig");
    _ = @import("tools/file.zig");
}
