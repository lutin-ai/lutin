const std = @import("std");
const Allocator = std.mem.Allocator;
const Io = std.Io;

pub const Header = struct {
    name: []const u8,
    value: []const u8,
};

pub const Response = struct {
    status: u16,
    body: []u8,
};

pub const SseEvent = struct {
    event: ?[]const u8,
    data: []const u8,
};

pub const HttpError = error{ Network, Tls, BadStatus, Truncated, OutOfMemory };

const max_body = 16 * 1024 * 1024;
const max_sse_line = 1 << 20;

pub fn postJson(
    alloc: Allocator,
    io: Io,
    url: []const u8,
    headers: []const Header,
    body_json: []const u8,
) HttpError!Response {
    return sendWithDetail(alloc, io, .POST, url, headers, body_json, null);
}

pub fn getJson(
    alloc: Allocator,
    io: Io,
    url: []const u8,
    headers: []const Header,
) HttpError!Response {
    return sendWithDetail(alloc, io, .GET, url, headers, null, null);
}

pub fn postJsonDetail(
    alloc: Allocator,
    io: Io,
    url: []const u8,
    headers: []const Header,
    body_json: []const u8,
    detail_out: ?*?[]u8,
) HttpError!Response {
    return sendWithDetail(alloc, io, .POST, url, headers, body_json, detail_out);
}

fn sendWithDetail(
    alloc: Allocator,
    io: Io,
    method: std.http.Method,
    url: []const u8,
    headers: []const Header,
    body_json: ?[]const u8,
    detail_out: ?*?[]u8,
) HttpError!Response {
    var client: std.http.Client = .{ .allocator = alloc, .io = io };
    defer client.deinit();

    const uri = std.Uri.parse(url) catch return error.Network;

    const extra = buildHeaders(alloc, headers, body_json != null) catch return error.OutOfMemory;
    defer alloc.free(extra);

    var req = client.request(method, uri, .{
        .extra_headers = extra,
        .redirect_behavior = if (body_json == null) @enumFromInt(3) else .unhandled,
        .headers = .{ .accept_encoding = .omit },
    }) catch |e| return mapReq(e);
    defer req.deinit();

    if (body_json) |payload| {
        req.transfer_encoding = .{ .content_length = payload.len };
        var bw = req.sendBodyUnflushed(&.{}) catch return error.Network;
        bw.writer.writeAll(payload) catch return error.Network;
        bw.end() catch return error.Network;
        req.connection.?.flush() catch return error.Network;
    } else {
        req.sendBodiless() catch return error.Network;
    }

    var redirect_buf: [8 * 1024]u8 = undefined;
    var response = req.receiveHead(&redirect_buf) catch |e| return mapHead(e);

    const status = @intFromEnum(response.head.status);

    const reader = response.reader(&.{});
    const body = reader.allocRemaining(alloc, .limited(max_body)) catch |e| switch (e) {
        error.OutOfMemory => return error.OutOfMemory,
        error.StreamTooLong => return error.Truncated,
        error.ReadFailed => return error.Network,
    };
    errdefer alloc.free(body);

    if (status >= 400) {
        if (detail_out) |out| {
            out.* = formatStatusDetail(alloc, status, body) catch null;
        }
        alloc.free(body);
        return error.BadStatus;
    }
    return .{ .status = status, .body = body };
}

fn formatStatusDetail(alloc: Allocator, status: u16, body: []const u8) ![]u8 {
    const max_snippet: usize = 1024;
    const snippet = body[0..@min(body.len, max_snippet)];
    const ellipsis = if (body.len > max_snippet) "..." else "";
    return std.fmt.allocPrint(alloc, "HTTP {d}: {s}{s}", .{ status, snippet, ellipsis });
}

fn buildHeaders(alloc: Allocator, headers: []const Header, has_body: bool) ![]std.http.Header {
    const extra_count = headers.len + (if (has_body) @as(usize, 1) else 0);
    const out = try alloc.alloc(std.http.Header, extra_count);
    var i: usize = 0;
    if (has_body) {
        out[i] = .{ .name = "content-type", .value = "application/json" };
        i += 1;
    }
    for (headers) |h| {
        out[i] = .{ .name = h.name, .value = h.value };
        i += 1;
    }
    return out;
}

fn mapReq(e: anyerror) HttpError {
    return switch (e) {
        error.OutOfMemory => error.OutOfMemory,
        error.CertificateBundleLoadFailure => error.Tls,
        else => error.Network,
    };
}

fn mapHead(e: anyerror) HttpError {
    return switch (e) {
        error.OutOfMemory => error.OutOfMemory,
        error.CertificateBundleLoadFailure => error.Tls,
        error.HttpHeadersOversize, error.HttpChunkTruncated => error.Truncated,
        else => error.Network,
    };
}

pub fn streamSse(
    alloc: Allocator,
    io: Io,
    url: []const u8,
    headers: []const Header,
    body_json: []const u8,
    ctx: anytype,
    comptime onEvent: fn (@TypeOf(ctx), event: SseEvent) anyerror!void,
) HttpError!void {
    return streamSseDetail(alloc, io, url, headers, body_json, ctx, onEvent, null);
}

pub fn streamSseDetail(
    alloc: Allocator,
    io: Io,
    url: []const u8,
    headers: []const Header,
    body_json: []const u8,
    ctx: anytype,
    comptime onEvent: fn (@TypeOf(ctx), event: SseEvent) anyerror!void,
    detail_out: ?*?[]u8,
) HttpError!void {
    var client: std.http.Client = .{ .allocator = alloc, .io = io };
    defer client.deinit();

    const uri = std.Uri.parse(url) catch return error.Network;

    var extra_list = std.array_list.Managed(std.http.Header).init(alloc);
    defer extra_list.deinit();
    extra_list.append(.{ .name = "content-type", .value = "application/json" }) catch return error.OutOfMemory;
    extra_list.append(.{ .name = "accept", .value = "text/event-stream" }) catch return error.OutOfMemory;
    for (headers) |h| extra_list.append(.{ .name = h.name, .value = h.value }) catch return error.OutOfMemory;

    var req = client.request(.POST, uri, .{
        .extra_headers = extra_list.items,
        .redirect_behavior = .unhandled,
        .headers = .{ .accept_encoding = .omit },
    }) catch |e| return mapReq(e);
    defer req.deinit();

    req.transfer_encoding = .{ .content_length = body_json.len };
    var bw = req.sendBodyUnflushed(&.{}) catch return error.Network;
    bw.writer.writeAll(body_json) catch return error.Network;
    bw.end() catch return error.Network;
    req.connection.?.flush() catch return error.Network;

    var redirect_buf: [8 * 1024]u8 = undefined;
    var response = req.receiveHead(&redirect_buf) catch |e| return mapHead(e);

    const sse_status = @intFromEnum(response.head.status);
    var xfer_buf: [16 * 1024]u8 = undefined;
    const r = response.reader(&xfer_buf);
    if (sse_status >= 400) {
        if (detail_out) |out| {
            const body = r.allocRemaining(alloc, .limited(max_body)) catch |e| switch (e) {
                error.OutOfMemory => return error.OutOfMemory,
                error.StreamTooLong => return error.Truncated,
                error.ReadFailed => return error.Network,
            };
            defer alloc.free(body);
            out.* = formatStatusDetail(alloc, sse_status, body) catch null;
        }
        return error.BadStatus;
    }
    try parseSse(r, ctx, onEvent);
}

fn parseSse(
    r: *Io.Reader,
    ctx: anytype,
    comptime onEvent: fn (@TypeOf(ctx), event: SseEvent) anyerror!void,
) HttpError!void {
    var data_buf: std.array_list.Managed(u8) = .init(std.heap.page_allocator);
    defer data_buf.deinit();
    var line_buf: std.array_list.Managed(u8) = .init(std.heap.page_allocator);
    defer line_buf.deinit();
    var event_name_buf: [256]u8 = undefined;
    var event_len: ?usize = null;
    var eos = false;

    while (!eos) {
        line_buf.clearRetainingCapacity();
        while (true) {
            const b = r.takeByte() catch |e| switch (e) {
                error.EndOfStream => {
                    eos = true;
                    break;
                },
                error.ReadFailed => return error.Network,
            };
            if (b == '\n') break;
            if (line_buf.items.len >= max_sse_line) return error.Truncated;
            line_buf.append(b) catch return error.OutOfMemory;
        }
        if (eos and line_buf.items.len == 0) break;

        const raw = line_buf.items;
        const trimmed = if (raw.len > 0 and raw[raw.len - 1] == '\r') raw[0 .. raw.len - 1] else raw;

        if (trimmed.len == 0) {
            if (data_buf.items.len != 0 or event_len != null) {
                try deliver(ctx, onEvent, event_name_buf[0..(event_len orelse 0)], event_len, data_buf.items);
            }
            data_buf.clearRetainingCapacity();
            event_len = null;
            continue;
        }

        if (trimmed[0] == ':') continue;

        const colon = std.mem.indexOfScalar(u8, trimmed, ':');
        const field = if (colon) |c| trimmed[0..c] else trimmed;
        var value: []const u8 = if (colon) |c| trimmed[c + 1 ..] else "";
        if (value.len > 0 and value[0] == ' ') value = value[1..];

        if (std.mem.eql(u8, field, "data")) {
            if (data_buf.items.len != 0) data_buf.append('\n') catch return error.OutOfMemory;
            data_buf.appendSlice(value) catch return error.OutOfMemory;
        } else if (std.mem.eql(u8, field, "event")) {
            const n = @min(value.len, event_name_buf.len);
            @memcpy(event_name_buf[0..n], value[0..n]);
            event_len = n;
        }
    }

    if (data_buf.items.len != 0) {
        try deliver(ctx, onEvent, event_name_buf[0..(event_len orelse 0)], event_len, data_buf.items);
    }
}

fn deliver(
    ctx: anytype,
    comptime onEvent: fn (@TypeOf(ctx), event: SseEvent) anyerror!void,
    name_bytes: []const u8,
    has_name: ?usize,
    data: []const u8,
) HttpError!void {
    const ev: SseEvent = .{
        .event = if (has_name == null) null else name_bytes,
        .data = data,
    };
    onEvent(ctx, ev) catch return error.Network;
}

const TestEvent = struct { name: ?[]const u8, data: []const u8 };

const Collector = struct {
    events: std.array_list.Managed(TestEvent),
    alloc: Allocator,

    fn init(a: Allocator) Collector {
        return .{ .events = std.array_list.Managed(TestEvent).init(a), .alloc = a };
    }
    fn deinit(self: *Collector) void {
        for (self.events.items) |e| {
            self.alloc.free(e.data);
            if (e.name) |n| self.alloc.free(n);
        }
        self.events.deinit();
    }
    fn cb(self: *Collector, e: SseEvent) anyerror!void {
        const data_copy = try self.alloc.dupe(u8, e.data);
        const name_copy = if (e.event) |n| try self.alloc.dupe(u8, n) else null;
        try self.events.append(.{ .name = name_copy, .data = data_copy });
    }
};

test "sse parses multi-line data and concatenates with \\n" {
    const stream =
        "event: message\n" ++
        "data: hello\n" ++
        "data: world\n" ++
        "\n" ++
        "data: [DONE]\n" ++
        "\n";
    var r: Io.Reader = .fixed(stream);

    var c = Collector.init(std.testing.allocator);
    defer c.deinit();
    try parseSse(&r, &c, Collector.cb);

    try std.testing.expectEqual(@as(usize, 2), c.events.items.len);
    try std.testing.expectEqualStrings("message", c.events.items[0].name.?);
    try std.testing.expectEqualStrings("hello\nworld", c.events.items[0].data);
    try std.testing.expectEqual(@as(?[]const u8, null), c.events.items[1].name);
    try std.testing.expectEqualStrings("[DONE]", c.events.items[1].data);
}

test "sse handles CRLF and comment lines" {
    const stream =
        ": comment line\r\n" ++
        "data: a\r\n" ++
        "\r\n";
    var r: Io.Reader = .fixed(stream);

    var c = Collector.init(std.testing.allocator);
    defer c.deinit();
    try parseSse(&r, &c, Collector.cb);
    try std.testing.expectEqual(@as(usize, 1), c.events.items.len);
    try std.testing.expectEqualStrings("a", c.events.items[0].data);
}

test "sse empty input yields no events" {
    var r: Io.Reader = .fixed("");
    var c = Collector.init(std.testing.allocator);
    defer c.deinit();
    try parseSse(&r, &c, Collector.cb);
    try std.testing.expectEqual(@as(usize, 0), c.events.items.len);
}
