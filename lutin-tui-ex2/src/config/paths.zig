const std = @import("std");
const Io = std.Io;
const Dir = std.Io.Dir;
const Map = std.process.Environ.Map;

pub const Error = error{HomeNotSet} || Dir.CreateDirPathError || std.mem.Allocator.Error;

fn join(alloc: std.mem.Allocator, a: []const u8, b: []const u8) ![]u8 {
    return std.fs.path.join(alloc, &.{ a, b });
}

fn xdgOrHome(alloc: std.mem.Allocator, env: *const Map, xdg_var: []const u8, home_rel: []const u8) ![]u8 {
    if (env.get(xdg_var)) |v| {
        if (v.len > 0) return join(alloc, v, "lutin");
    }
    const home = env.get("HOME") orelse return error.HomeNotSet;
    return join(alloc, home, home_rel);
}

pub fn configDir(io: Io, alloc: std.mem.Allocator, env: *const Map) ![]u8 {
    const dir = try xdgOrHome(alloc, env, "XDG_CONFIG_HOME", ".config/lutin");
    errdefer alloc.free(dir);
    try Dir.cwd().createDirPath(io, dir);
    return dir;
}

pub fn dataDir(io: Io, alloc: std.mem.Allocator, env: *const Map) ![]u8 {
    const dir = try xdgOrHome(alloc, env, "XDG_DATA_HOME", ".local/share/lutin");
    errdefer alloc.free(dir);
    try Dir.cwd().createDirPath(io, dir);
    return dir;
}

pub fn sessionsDir(io: Io, alloc: std.mem.Allocator, env: *const Map) ![]u8 {
    const data = try dataDir(io, alloc, env);
    defer alloc.free(data);
    const dir = try join(alloc, data, "sessions");
    errdefer alloc.free(dir);
    try Dir.cwd().createDirPath(io, dir);
    return dir;
}

pub fn settingsPath(io: Io, alloc: std.mem.Allocator, env: *const Map) ![]u8 {
    const cfg = try configDir(io, alloc, env);
    defer alloc.free(cfg);
    return join(alloc, cfg, "settings.json");
}

pub fn personasPath(io: Io, alloc: std.mem.Allocator, env: *const Map) ![]u8 {
    const cfg = try configDir(io, alloc, env);
    defer alloc.free(cfg);
    return join(alloc, cfg, "personas.json");
}

test "configDir returns non-empty path under XDG_CONFIG_HOME" {
    const testing = std.testing;
    var tmp = testing.tmpDir(.{});
    defer tmp.cleanup();

    var buf: [Dir.max_path_bytes]u8 = undefined;
    const len = try tmp.dir.realPath(testing.io, &buf);
    const tmp_path = buf[0..len];

    var env: Map = .init(testing.allocator);
    defer env.deinit();
    try env.put("XDG_CONFIG_HOME", tmp_path);
    try env.put("HOME", tmp_path);

    const got = try configDir(testing.io, testing.allocator, &env);
    defer testing.allocator.free(got);
    try testing.expect(got.len > 0);
    try testing.expect(std.mem.endsWith(u8, got, "lutin"));
}
