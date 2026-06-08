const std = @import("std");
const Io = std.Io;
const Map = std.process.Environ.Map;

const paths = @import("paths.zig");
const jsonfile = @import("jsonfile.zig");

pub const Settings = struct {
    base_url: []const u8,
    api_key_env: []const u8,
    model: []const u8,
    persona: []const u8,
    temperature: ?f32 = null,
    max_tokens: ?u32 = null,
};

pub const Loaded = struct {
    value: Settings,
    arena: *std.heap.ArenaAllocator,
    was_created: bool,

    pub fn deinit(self: *Loaded, alloc: std.mem.Allocator) void {
        self.arena.deinit();
        alloc.destroy(self.arena);
    }
};

const stub_json =
    \\{
    \\  "base_url": "https://api.openai.com/v1",
    \\  "api_key_env": "OPENAI_API_KEY",
    \\  "model": "gpt-4o-mini",
    \\  "persona": "default",
    \\  "temperature": null,
    \\  "max_tokens": null
    \\}
    \\
;

pub fn loadOrCreate(io: Io, alloc: std.mem.Allocator, env: *const Map) !Loaded {
    const path = try paths.settingsPath(io, alloc, env);
    defer alloc.free(path);

    var was_created = false;
    const bytes = jsonfile.readAlloc(io, alloc, path) catch |err| switch (err) {
        error.FileNotFound => blk: {
            try jsonfile.writeAtomic(io, alloc, path, stub_json);
            was_created = true;
            break :blk try alloc.dupe(u8, stub_json);
        },
        else => return err,
    };
    defer alloc.free(bytes);

    const arena = try alloc.create(std.heap.ArenaAllocator);
    errdefer alloc.destroy(arena);
    arena.* = .init(alloc);
    errdefer arena.deinit();

    const aalloc = arena.allocator();

    const parsed = try std.json.parseFromSliceLeaky(Settings, aalloc, bytes, .{
        .ignore_unknown_fields = true,
        .allocate = .alloc_always,
    });

    return .{ .value = parsed, .arena = arena, .was_created = was_created };
}

pub fn resolveApiKey(env: *const Map, name: []const u8) []const u8 {
    if (env.get(name)) |v| return v;
    return "";
}
