const std = @import("std");
const Io = std.Io;
const Dir = std.Io.Dir;
const types = @import("../llm/types.zig");
const loop = @import("../agent/loop.zig");

pub const FileToolCtx = struct { io: Io };

fn dupOk(alloc: std.mem.Allocator, s: []const u8) anyerror!loop.ExecResult {
    return .{ .output = try alloc.dupe(u8, s), .is_error = false };
}

fn dupErr(alloc: std.mem.Allocator, s: []const u8) anyerror!loop.ExecResult {
    return .{ .output = try alloc.dupe(u8, s), .is_error = true };
}

// path sandbox: forbid traversal and obvious system paths until the approval flow is wired end-to-end
fn pathAllowed(p: []const u8) bool {
    if (p.len == 0) return false;
    if (p[0] == '~') return false;
    if (std.mem.startsWith(u8, p, "/etc") or
        std.mem.startsWith(u8, p, "/proc") or
        std.mem.startsWith(u8, p, "/sys") or
        std.mem.startsWith(u8, p, "/dev")) return false;
    var it = std.mem.splitScalar(u8, p, '/');
    while (it.next()) |seg| {
        if (std.mem.eql(u8, seg, "..")) return false;
    }
    return true;
}

// ---------------- file_read ----------------

const ReadArgs = struct {
    path: []const u8,
    max_bytes: ?u64 = null,
};

fn runRead(ctx: *anyopaque, alloc: std.mem.Allocator, args_json: []const u8) anyerror!loop.ExecResult {
    const fctx: *FileToolCtx = @ptrCast(@alignCast(ctx));
    var parsed = std.json.parseFromSlice(ReadArgs, alloc, args_json, .{ .ignore_unknown_fields = true }) catch
        return dupErr(alloc, "invalid arguments");
    defer parsed.deinit();
    const a = parsed.value;

    const cap: u64 = a.max_bytes orelse (200 * 1024);
    const io = fctx.io;
    const bytes = Dir.cwd().readFileAlloc(io, a.path, alloc, .limited64(cap)) catch |err| switch (err) {
        error.StreamTooLong => return dupErr(alloc, "file exceeds max_bytes cap"),
        error.FileNotFound => return dupErr(alloc, "file not found"),
        error.OutOfMemory => return err,
        else => return dupErr(alloc, "read failed"),
    };
    return .{ .output = bytes, .is_error = false };
}

// ---------------- file_write ----------------

const WriteArgs = struct {
    path: []const u8,
    content: []const u8,
};

fn ensureParentDirs(io: Io, path: []const u8) !void {
    const dirname = std.fs.path.dirname(path) orelse return;
    if (dirname.len == 0) return;
    Dir.cwd().createDirPath(io, dirname) catch |err| switch (err) {
        error.PathAlreadyExists => return,
        else => return err,
    };
}

fn runWrite(ctx: *anyopaque, alloc: std.mem.Allocator, args_json: []const u8) anyerror!loop.ExecResult {
    const fctx: *FileToolCtx = @ptrCast(@alignCast(ctx));
    var parsed = std.json.parseFromSlice(WriteArgs, alloc, args_json, .{ .ignore_unknown_fields = true }) catch
        return dupErr(alloc, "invalid arguments");
    defer parsed.deinit();
    const a = parsed.value;

    if (!pathAllowed(a.path)) return dupErr(alloc, "path not allowed");

    const io = fctx.io;
    ensureParentDirs(io, a.path) catch return dupErr(alloc, "failed to create parent dirs");
    Dir.cwd().writeFile(io, .{ .sub_path = a.path, .data = a.content, .flags = .{ .truncate = true } }) catch
        return dupErr(alloc, "write failed");

    const out = try std.fmt.allocPrint(alloc, "wrote {d} bytes to {s}", .{ a.content.len, a.path });
    return .{ .output = out, .is_error = false };
}

// ---------------- file_edit ----------------

const EditArgs = struct {
    path: []const u8,
    old_string: []const u8,
    new_string: []const u8,
};

fn runEdit(ctx: *anyopaque, alloc: std.mem.Allocator, args_json: []const u8) anyerror!loop.ExecResult {
    const fctx: *FileToolCtx = @ptrCast(@alignCast(ctx));
    var parsed = std.json.parseFromSlice(EditArgs, alloc, args_json, .{ .ignore_unknown_fields = true }) catch
        return dupErr(alloc, "invalid arguments");
    defer parsed.deinit();
    const a = parsed.value;

    if (!pathAllowed(a.path)) return dupErr(alloc, "path not allowed");

    const io = fctx.io;
    const bytes = Dir.cwd().readFileAlloc(io, a.path, alloc, .unlimited) catch |err| switch (err) {
        error.FileNotFound => return dupErr(alloc, "file not found"),
        error.OutOfMemory => return err,
        else => return dupErr(alloc, "read failed"),
    };
    defer alloc.free(bytes);

    if (a.old_string.len == 0) return dupErr(alloc, "old_string is empty");

    var count: usize = 0;
    var first: usize = 0;
    var i: usize = 0;
    while (std.mem.indexOfPos(u8, bytes, i, a.old_string)) |pos| {
        if (count == 0) first = pos;
        count += 1;
        i = pos + a.old_string.len;
        if (count > 1) break;
    }
    if (count == 0) return dupErr(alloc, "old_string not found");
    if (count > 1) return dupErr(alloc, "old_string is not unique");

    var out_buf: std.ArrayList(u8) = .empty;
    defer out_buf.deinit(alloc);
    try out_buf.appendSlice(alloc, bytes[0..first]);
    try out_buf.appendSlice(alloc, a.new_string);
    try out_buf.appendSlice(alloc, bytes[first + a.old_string.len ..]);

    Dir.cwd().writeFile(io, .{ .sub_path = a.path, .data = out_buf.items, .flags = .{ .truncate = true } }) catch
        return dupErr(alloc, "write failed");

    const out = try std.fmt.allocPrint(alloc, "edited {s}", .{a.path});
    return .{ .output = out, .is_error = false };
}

// ---------------- file_grep ----------------

const GrepArgs = struct {
    pattern: []const u8,
    path: ?[]const u8 = null,
    max_matches: ?u32 = null,
};

fn appendGrepInFile(
    alloc: std.mem.Allocator,
    io: Io,
    rel_path: []const u8,
    pattern: []const u8,
    out: *std.ArrayList(u8),
    matches_left: *u32,
) !void {
    const bytes = Dir.cwd().readFileAlloc(io, rel_path, alloc, .limited64(2 * 1024 * 1024)) catch return;
    defer alloc.free(bytes);

    var line_no: u32 = 0;
    var it = std.mem.splitScalar(u8, bytes, '\n');
    while (it.next()) |line| {
        line_no += 1;
        if (std.mem.indexOf(u8, line, pattern) != null) {
            const formatted = try std.fmt.allocPrint(alloc, "{s}:{d}:{s}\n", .{ rel_path, line_no, line });
            defer alloc.free(formatted);
            try out.appendSlice(alloc, formatted);
            matches_left.* -= 1;
            if (matches_left.* == 0) return;
        }
    }
}

fn runGrep(ctx: *anyopaque, alloc: std.mem.Allocator, args_json: []const u8) anyerror!loop.ExecResult {
    const fctx: *FileToolCtx = @ptrCast(@alignCast(ctx));
    var parsed = std.json.parseFromSlice(GrepArgs, alloc, args_json, .{ .ignore_unknown_fields = true }) catch
        return dupErr(alloc, "invalid arguments");
    defer parsed.deinit();
    const a = parsed.value;

    const root_path = a.path orelse ".";
    var max: u32 = a.max_matches orelse 200;

    const io = fctx.io;
    var root = Dir.cwd().openDir(io, root_path, .{ .iterate = true }) catch
        return dupErr(alloc, "cannot open path");
    defer root.close(io);

    var walker = try root.walk(alloc);
    defer walker.deinit();

    var out: std.ArrayList(u8) = .empty;
    defer out.deinit(alloc);

    while (try walker.next(io)) |entry| {
        if (entry.kind != .file) continue;
        const full = try std.fs.path.join(alloc, &.{ root_path, entry.path });
        defer alloc.free(full);
        try appendGrepInFile(alloc, io, full, a.pattern, &out, &max);
        if (max == 0) break;
    }

    return .{ .output = try alloc.dupe(u8, out.items), .is_error = false };
}

// ---------------- file_glob ----------------

const GlobArgs = struct {
    pattern: []const u8,
    path: ?[]const u8 = null,
};

fn globMatch(pattern: []const u8, name: []const u8) bool {
    return matchHelper(pattern, 0, name, 0);
}

fn matchHelper(pattern: []const u8, pi: usize, name: []const u8, ni: usize) bool {
    var p = pi;
    var n = ni;
    while (p < pattern.len) {
        const c = pattern[p];
        if (c == '*') {
            if (p + 1 == pattern.len) return true;
            while (n <= name.len) : (n += 1) {
                if (matchHelper(pattern, p + 1, name, n)) return true;
            }
            return false;
        } else if (c == '?') {
            if (n >= name.len) return false;
            p += 1;
            n += 1;
        } else {
            if (n >= name.len or name[n] != c) return false;
            p += 1;
            n += 1;
        }
    }
    return n == name.len;
}

fn runGlob(ctx: *anyopaque, alloc: std.mem.Allocator, args_json: []const u8) anyerror!loop.ExecResult {
    const fctx: *FileToolCtx = @ptrCast(@alignCast(ctx));
    var parsed = std.json.parseFromSlice(GlobArgs, alloc, args_json, .{ .ignore_unknown_fields = true }) catch
        return dupErr(alloc, "invalid arguments");
    defer parsed.deinit();
    const a = parsed.value;

    const root_path = a.path orelse ".";
    const io = fctx.io;
    var root = Dir.cwd().openDir(io, root_path, .{ .iterate = true }) catch
        return dupErr(alloc, "cannot open path");
    defer root.close(io);

    var walker = try root.walk(alloc);
    defer walker.deinit();

    var out: std.ArrayList(u8) = .empty;
    defer out.deinit(alloc);

    while (try walker.next(io)) |entry| {
        if (entry.kind != .file) continue;
        if (globMatch(a.pattern, entry.basename)) {
            try out.appendSlice(alloc, entry.path);
            try out.append(alloc, '\n');
        }
    }

    return .{ .output = try alloc.dupe(u8, out.items), .is_error = false };
}

// ---------------- file_list ----------------

const ListArgs = struct {
    path: ?[]const u8 = null,
};

fn runList(ctx: *anyopaque, alloc: std.mem.Allocator, args_json: []const u8) anyerror!loop.ExecResult {
    const fctx: *FileToolCtx = @ptrCast(@alignCast(ctx));
    var parsed = std.json.parseFromSlice(ListArgs, alloc, args_json, .{ .ignore_unknown_fields = true }) catch
        return dupErr(alloc, "invalid arguments");
    defer parsed.deinit();
    const a = parsed.value;

    const path = a.path orelse ".";
    const io = fctx.io;
    var dir = Dir.cwd().openDir(io, path, .{ .iterate = true }) catch
        return dupErr(alloc, "cannot open path");
    defer dir.close(io);

    var out: std.ArrayList(u8) = .empty;
    defer out.deinit(alloc);

    var it = dir.iterate();
    while (try it.next(io)) |entry| {
        try out.appendSlice(alloc, entry.name);
        if (entry.kind == .directory) try out.append(alloc, '/');
        try out.append(alloc, '\n');
    }

    return .{ .output = try alloc.dupe(u8, out.items), .is_error = false };
}

// ---------------- file_tree ----------------

const TreeArgs = struct {
    path: ?[]const u8 = null,
    max_depth: ?u32 = null,
};

fn renderTree(
    alloc: std.mem.Allocator,
    io: Io,
    dir: Dir,
    out: *std.ArrayList(u8),
    prefix: []const u8,
    depth: u32,
    max_depth: u32,
) !void {
    if (depth >= max_depth) return;

    var names: std.ArrayList([]u8) = .empty;
    defer {
        for (names.items) |n| alloc.free(n);
        names.deinit(alloc);
    }
    var kinds: std.ArrayList(std.Io.File.Kind) = .empty;
    defer kinds.deinit(alloc);

    var it = dir.iterate();
    while (try it.next(io)) |entry| {
        try names.append(alloc, try alloc.dupe(u8, entry.name));
        try kinds.append(alloc, entry.kind);
    }

    for (names.items, 0..) |name, i| {
        const last = (i == names.items.len - 1);
        const branch = if (last) "`-- " else "|-- ";
        try out.appendSlice(alloc, prefix);
        try out.appendSlice(alloc, branch);
        try out.appendSlice(alloc, name);
        if (kinds.items[i] == .directory) try out.append(alloc, '/');
        try out.append(alloc, '\n');

        if (kinds.items[i] == .directory and depth + 1 < max_depth) {
            const continuation = if (last) "    " else "|   ";
            const new_prefix = try std.mem.concat(alloc, u8, &.{ prefix, continuation });
            defer alloc.free(new_prefix);
            var sub = dir.openDir(io, name, .{ .iterate = true }) catch continue;
            defer sub.close(io);
            try renderTree(alloc, io, sub, out, new_prefix, depth + 1, max_depth);
        }
    }
}

fn runTree(ctx: *anyopaque, alloc: std.mem.Allocator, args_json: []const u8) anyerror!loop.ExecResult {
    const fctx: *FileToolCtx = @ptrCast(@alignCast(ctx));
    var parsed = std.json.parseFromSlice(TreeArgs, alloc, args_json, .{ .ignore_unknown_fields = true }) catch
        return dupErr(alloc, "invalid arguments");
    defer parsed.deinit();
    const a = parsed.value;

    const path = a.path orelse ".";
    const depth = a.max_depth orelse 3;

    const io = fctx.io;
    var dir = Dir.cwd().openDir(io, path, .{ .iterate = true }) catch
        return dupErr(alloc, "cannot open path");
    defer dir.close(io);

    var out: std.ArrayList(u8) = .empty;
    defer out.deinit(alloc);

    try out.appendSlice(alloc, path);
    try out.append(alloc, '\n');
    try renderTree(alloc, io, dir, &out, "", 0, depth);

    return .{ .output = try alloc.dupe(u8, out.items), .is_error = false };
}

// ---------------- registry ----------------

fn dupParam(
    alloc: std.mem.Allocator,
    name: []const u8,
    type_name: []const u8,
    description: []const u8,
    required: bool,
) !types.ToolParameter {
    return .{
        .name = try alloc.dupe(u8, name),
        .type_name = try alloc.dupe(u8, type_name),
        .description = try alloc.dupe(u8, description),
        .required = required,
    };
}

fn buildDef(
    alloc: std.mem.Allocator,
    name: []const u8,
    description: []const u8,
    params: []const types.ToolParameter,
) !types.ToolDefinition {
    const params_dup = try alloc.alloc(types.ToolParameter, params.len);
    @memcpy(params_dup, params);
    return .{
        .name = try alloc.dupe(u8, name),
        .description = try alloc.dupe(u8, description),
        .parameters = params_dup,
    };
}

pub fn allFileTools(alloc: std.mem.Allocator, ctx: *FileToolCtx) ![]loop.ToolExec.Tool {
    var tools: std.ArrayList(loop.ToolExec.Tool) = .empty;
    errdefer tools.deinit(alloc);

    {
        const params = [_]types.ToolParameter{
            try dupParam(alloc, "path", "string", "File path to read.", true),
            try dupParam(alloc, "max_bytes", "number", "Max bytes to read (default 200KB).", false),
        };
        try tools.append(alloc, .{
            .def = try buildDef(alloc, "file_read", "Read a UTF-8 file from disk.", &params),
            .run = runRead,
            .ctx = @ptrCast(ctx),
        });
    }
    {
        const params = [_]types.ToolParameter{
            try dupParam(alloc, "path", "string", "File path to write.", true),
            try dupParam(alloc, "content", "string", "Content to write.", true),
        };
        try tools.append(alloc, .{
            .def = try buildDef(alloc, "file_write", "Overwrite a file, creating parent dirs.", &params),
            .run = runWrite,
            .ctx = @ptrCast(ctx),
        });
    }
    {
        const params = [_]types.ToolParameter{
            try dupParam(alloc, "path", "string", "File path to edit.", true),
            try dupParam(alloc, "old_string", "string", "Exact substring to replace; must occur once.", true),
            try dupParam(alloc, "new_string", "string", "Replacement text.", true),
        };
        try tools.append(alloc, .{
            .def = try buildDef(alloc, "file_edit", "Replace a unique substring in a file.", &params),
            .run = runEdit,
            .ctx = @ptrCast(ctx),
        });
    }
    {
        const params = [_]types.ToolParameter{
            try dupParam(alloc, "pattern", "string", "Substring to search for.", true),
            try dupParam(alloc, "path", "string", "Root directory (default '.').", false),
            try dupParam(alloc, "max_matches", "number", "Cap on matches (default 200).", false),
        };
        try tools.append(alloc, .{
            .def = try buildDef(alloc, "file_grep", "Recursive substring search; emits path:line:match.", &params),
            .run = runGrep,
            .ctx = @ptrCast(ctx),
        });
    }
    {
        const params = [_]types.ToolParameter{
            try dupParam(alloc, "pattern", "string", "Glob with * and ? (no **).", true),
            try dupParam(alloc, "path", "string", "Root directory (default '.').", false),
        };
        try tools.append(alloc, .{
            .def = try buildDef(alloc, "file_glob", "Recursive glob match against basenames.", &params),
            .run = runGlob,
            .ctx = @ptrCast(ctx),
        });
    }
    {
        const params = [_]types.ToolParameter{
            try dupParam(alloc, "path", "string", "Directory to list (default '.').", false),
        };
        try tools.append(alloc, .{
            .def = try buildDef(alloc, "file_list", "Non-recursive ls.", &params),
            .run = runList,
            .ctx = @ptrCast(ctx),
        });
    }
    {
        const params = [_]types.ToolParameter{
            try dupParam(alloc, "path", "string", "Directory root (default '.').", false),
            try dupParam(alloc, "max_depth", "number", "Max depth (default 3).", false),
        };
        try tools.append(alloc, .{
            .def = try buildDef(alloc, "file_tree", "ASCII tree of a directory.", &params),
            .run = runTree,
            .ctx = @ptrCast(ctx),
        });
    }

    return tools.toOwnedSlice(alloc);
}

pub fn freeTools(alloc: std.mem.Allocator, tools: []loop.ToolExec.Tool) void {
    for (tools) |t| {
        for (t.def.parameters) |p| {
            alloc.free(p.name);
            alloc.free(p.type_name);
            alloc.free(p.description);
        }
        alloc.free(t.def.parameters);
        alloc.free(t.def.name);
        alloc.free(t.def.description);
    }
    alloc.free(tools);
}

// ---------------- tests ----------------

const testing = std.testing;

fn findTool(tools: []loop.ToolExec.Tool, name: []const u8) *loop.ToolExec.Tool {
    for (tools) |*t| {
        if (std.mem.eql(u8, t.def.name, name)) return t;
    }
    unreachable;
}

test "file_read happy path" {
    const alloc = testing.allocator;
    var ctx: FileToolCtx = .{ .io = testing.io };

    var tmp = testing.tmpDir(.{});
    defer tmp.cleanup();

    try tmp.dir.writeFile(testing.io, .{ .sub_path = "hello.txt", .data = "world", .flags = .{ .truncate = true } });

    const dir_rel = try std.fmt.allocPrint(alloc, ".zig-cache/tmp/{s}", .{tmp.sub_path[0..]});
    defer alloc.free(dir_rel);
    const full = try std.fs.path.join(alloc, &.{ dir_rel, "hello.txt" });
    defer alloc.free(full);

    const tools = try allFileTools(alloc, &ctx);
    defer freeTools(alloc, tools);
    const tool = findTool(tools, "file_read");

    const args = try std.fmt.allocPrint(alloc, "{{\"path\":\"{s}\"}}", .{full});
    defer alloc.free(args);

    const res = try tool.run(tool.ctx, alloc, args);
    defer alloc.free(res.output);

    try testing.expect(!res.is_error);
    try testing.expectEqualStrings("world", res.output);
}

test "file_write then re-read" {
    const alloc = testing.allocator;
    var ctx: FileToolCtx = .{ .io = testing.io };

    var tmp = testing.tmpDir(.{});
    defer tmp.cleanup();

    const dir_rel = try std.fmt.allocPrint(alloc, ".zig-cache/tmp/{s}", .{tmp.sub_path[0..]});
    defer alloc.free(dir_rel);
    const full = try std.fs.path.join(alloc, &.{ dir_rel, "out.txt" });
    defer alloc.free(full);

    const tools = try allFileTools(alloc, &ctx);
    defer freeTools(alloc, tools);

    const write_tool = findTool(tools, "file_write");
    const wargs = try std.fmt.allocPrint(alloc, "{{\"path\":\"{s}\",\"content\":\"abc\"}}", .{full});
    defer alloc.free(wargs);
    const wres = try write_tool.run(write_tool.ctx, alloc, wargs);
    defer alloc.free(wres.output);
    try testing.expect(!wres.is_error);

    const read_tool = findTool(tools, "file_read");
    const rargs = try std.fmt.allocPrint(alloc, "{{\"path\":\"{s}\"}}", .{full});
    defer alloc.free(rargs);
    const rres = try read_tool.run(read_tool.ctx, alloc, rargs);
    defer alloc.free(rres.output);
    try testing.expect(!rres.is_error);
    try testing.expectEqualStrings("abc", rres.output);
}

test "file_edit replaces unique match; rejects ambiguous" {
    const alloc = testing.allocator;
    var ctx: FileToolCtx = .{ .io = testing.io };

    var tmp = testing.tmpDir(.{});
    defer tmp.cleanup();

    const dir_rel = try std.fmt.allocPrint(alloc, ".zig-cache/tmp/{s}", .{tmp.sub_path[0..]});
    defer alloc.free(dir_rel);
    const full = try std.fs.path.join(alloc, &.{ dir_rel, "e.txt" });
    defer alloc.free(full);

    try tmp.dir.writeFile(testing.io, .{ .sub_path = "e.txt", .data = "alpha BETA gamma", .flags = .{ .truncate = true } });

    const tools = try allFileTools(alloc, &ctx);
    defer freeTools(alloc, tools);
    const edit_tool = findTool(tools, "file_edit");

    const args_ok = try std.fmt.allocPrint(alloc, "{{\"path\":\"{s}\",\"old_string\":\"BETA\",\"new_string\":\"DELTA\"}}", .{full});
    defer alloc.free(args_ok);
    const res_ok = try edit_tool.run(edit_tool.ctx, alloc, args_ok);
    defer alloc.free(res_ok.output);
    try testing.expect(!res_ok.is_error);

    try tmp.dir.writeFile(testing.io, .{ .sub_path = "e.txt", .data = "foo foo", .flags = .{ .truncate = true } });
    const args_dup = try std.fmt.allocPrint(alloc, "{{\"path\":\"{s}\",\"old_string\":\"foo\",\"new_string\":\"bar\"}}", .{full});
    defer alloc.free(args_dup);
    const res_dup = try edit_tool.run(edit_tool.ctx, alloc, args_dup);
    defer alloc.free(res_dup.output);
    try testing.expect(res_dup.is_error);
}

test "file_grep finds known substring" {
    const alloc = testing.allocator;
    var ctx: FileToolCtx = .{ .io = testing.io };

    var tmp = testing.tmpDir(.{});
    defer tmp.cleanup();

    try tmp.dir.writeFile(testing.io, .{ .sub_path = "g.txt", .data = "one\nNEEDLE here\nthree\n", .flags = .{ .truncate = true } });

    const dir_rel = try std.fmt.allocPrint(alloc, ".zig-cache/tmp/{s}", .{tmp.sub_path[0..]});
    defer alloc.free(dir_rel);

    const tools = try allFileTools(alloc, &ctx);
    defer freeTools(alloc, tools);
    const grep = findTool(tools, "file_grep");

    const args = try std.fmt.allocPrint(alloc, "{{\"pattern\":\"NEEDLE\",\"path\":\"{s}\"}}", .{dir_rel});
    defer alloc.free(args);
    const res = try grep.run(grep.ctx, alloc, args);
    defer alloc.free(res.output);

    try testing.expect(!res.is_error);
    try testing.expect(std.mem.indexOf(u8, res.output, "NEEDLE here") != null);
    try testing.expect(std.mem.indexOf(u8, res.output, ":2:") != null);
}

test "file_glob matches *.zig" {
    const alloc = testing.allocator;
    var ctx: FileToolCtx = .{ .io = testing.io };

    var tmp = testing.tmpDir(.{});
    defer tmp.cleanup();

    try tmp.dir.writeFile(testing.io, .{ .sub_path = "a.zig", .data = "", .flags = .{ .truncate = true } });
    try tmp.dir.writeFile(testing.io, .{ .sub_path = "b.txt", .data = "", .flags = .{ .truncate = true } });
    try tmp.dir.writeFile(testing.io, .{ .sub_path = "c.zig", .data = "", .flags = .{ .truncate = true } });

    const dir_rel = try std.fmt.allocPrint(alloc, ".zig-cache/tmp/{s}", .{tmp.sub_path[0..]});
    defer alloc.free(dir_rel);

    const tools = try allFileTools(alloc, &ctx);
    defer freeTools(alloc, tools);
    const glob = findTool(tools, "file_glob");

    const args = try std.fmt.allocPrint(alloc, "{{\"pattern\":\"*.zig\",\"path\":\"{s}\"}}", .{dir_rel});
    defer alloc.free(args);
    const res = try glob.run(glob.ctx, alloc, args);
    defer alloc.free(res.output);

    try testing.expect(!res.is_error);
    try testing.expect(std.mem.indexOf(u8, res.output, "a.zig") != null);
    try testing.expect(std.mem.indexOf(u8, res.output, "c.zig") != null);
    try testing.expect(std.mem.indexOf(u8, res.output, "b.txt") == null);
}

test "file_list and file_tree smoke" {
    const alloc = testing.allocator;
    var ctx: FileToolCtx = .{ .io = testing.io };

    var tmp = testing.tmpDir(.{});
    defer tmp.cleanup();

    try tmp.dir.writeFile(testing.io, .{ .sub_path = "x", .data = "", .flags = .{ .truncate = true } });

    const dir_rel = try std.fmt.allocPrint(alloc, ".zig-cache/tmp/{s}", .{tmp.sub_path[0..]});
    defer alloc.free(dir_rel);

    const tools = try allFileTools(alloc, &ctx);
    defer freeTools(alloc, tools);

    const list = findTool(tools, "file_list");
    const largs = try std.fmt.allocPrint(alloc, "{{\"path\":\"{s}\"}}", .{dir_rel});
    defer alloc.free(largs);
    const lres = try list.run(list.ctx, alloc, largs);
    defer alloc.free(lres.output);
    try testing.expect(!lres.is_error);
    try testing.expect(std.mem.indexOf(u8, lres.output, "x") != null);

    const tree = findTool(tools, "file_tree");
    const targs = try std.fmt.allocPrint(alloc, "{{\"path\":\"{s}\"}}", .{dir_rel});
    defer alloc.free(targs);
    const tres = try tree.run(tree.ctx, alloc, targs);
    defer alloc.free(tres.output);
    try testing.expect(!tres.is_error);
    try testing.expect(std.mem.indexOf(u8, tres.output, "x") != null);
}

test "path sandbox rejects traversal" {
    const alloc = testing.allocator;
    var ctx: FileToolCtx = .{ .io = testing.io };

    const tools = try allFileTools(alloc, &ctx);
    defer freeTools(alloc, tools);

    const write_tool = findTool(tools, "file_write");
    const args =
        \\{"path":"../etc/passwd","content":"x"}
    ;
    const res = try write_tool.run(write_tool.ctx, alloc, args);
    defer alloc.free(res.output);
    try testing.expect(res.is_error);
    try testing.expectEqualStrings("path not allowed", res.output);
}
