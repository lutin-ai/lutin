const std = @import("std");
const Io = std.Io;
const Dir = std.Io.Dir;

pub const ReadError = Dir.ReadFileAllocError;
pub const WriteError = Dir.CreateFileAtomicError || std.Io.File.Writer.Error || Dir.RenameError;

pub fn readAlloc(io: Io, alloc: std.mem.Allocator, path: []const u8) ReadError![]u8 {
    return Dir.cwd().readFileAlloc(io, path, alloc, .unlimited);
}

pub fn writeAtomic(io: Io, alloc: std.mem.Allocator, path: []const u8, bytes: []const u8) !void {
    const tmp = try std.mem.concat(alloc, u8, &.{ path, ".tmp" });
    defer alloc.free(tmp);

    {
        var file = try Dir.cwd().createFile(io, tmp, .{ .truncate = true });
        defer file.close(io);
        try file.writeStreamingAll(io, bytes);
    }

    // rename = atomic on POSIX so a crash mid-write can't corrupt the file
    try Dir.rename(Dir.cwd(), tmp, Dir.cwd(), path, io);
}

pub fn parseJson(comptime T: type, alloc: std.mem.Allocator, bytes: []const u8) !std.json.Parsed(T) {
    return std.json.parseFromSlice(T, alloc, bytes, .{
        .ignore_unknown_fields = true,
        .allocate = .alloc_always,
    });
}

pub fn stringifyAlloc(alloc: std.mem.Allocator, value: anytype) ![]u8 {
    return std.json.Stringify.valueAlloc(alloc, value, .{ .whitespace = .indent_2 });
}

test "round-trip struct through stringify and parseJson" {
    const testing = std.testing;
    const S = struct {
        name: []const u8,
        count: u32,
        flag: bool,
    };
    const original: S = .{ .name = "lutin", .count = 7, .flag = true };

    const buf = try stringifyAlloc(testing.allocator, original);
    defer testing.allocator.free(buf);

    var parsed = try parseJson(S, testing.allocator, buf);
    defer parsed.deinit();

    try testing.expectEqualStrings(original.name, parsed.value.name);
    try testing.expectEqual(original.count, parsed.value.count);
    try testing.expectEqual(original.flag, parsed.value.flag);
}

test "parseJson ignores unknown fields" {
    const testing = std.testing;
    const S = struct { a: u32 };
    var parsed = try parseJson(S, testing.allocator,
        \\{"a": 1, "extra": "ok"}
    );
    defer parsed.deinit();
    try testing.expectEqual(@as(u32, 1), parsed.value.a);
}
