const std = @import("std");
const zdt = @import("zdt");

pub const Datetime = zdt.Datetime;
pub const Duration = zdt.Duration;
pub const Timezone = zdt.Timezone;
pub const RelativeDelta = zdt.Duration.RelativeDelta;
pub const Resolution = zdt.Duration.Resolution;
pub const Timespan = zdt.Duration.Timespan;
pub const Weekday = zdt.Datetime.Weekday;
pub const Month = zdt.Datetime.Month;
pub const Fields = zdt.Datetime.Fields;
pub const Error = zdt.ZdtError;

pub const utc: Timezone = Timezone.UTC;

pub fn now_utc(io: std.Io) Datetime {
    return Datetime.nowUTC(io);
}

pub fn now_in(io: std.Io, tz: *const Timezone) !Datetime {
    return Datetime.now(io, .{ .tz = tz });
}

pub fn now_local(io: std.Io, allocator: std.mem.Allocator) !struct { dt: Datetime, tz: Timezone } {
    var tz = try Timezone.tzLocal(io, allocator);
    errdefer tz.deinit();
    const dt = try Datetime.now(io, .{ .tz = &tz });
    return .{ .dt = dt, .tz = tz };
}

pub fn load_tz(io: std.Io, allocator: std.mem.Allocator, identifier: []const u8) !Timezone {
    return Timezone.fromTzdata(io, identifier, allocator);
}

pub fn from_unix_seconds(seconds: i64, tz: ?*const Timezone) !Datetime {
    const opts: ?Datetime.tz_options = if (tz) |t| .{ .tz = t } else null;
    return Datetime.fromUnix(seconds, .second, opts);
}

pub fn from_unix_nanos(nanos: i128, tz: ?*const Timezone) !Datetime {
    const opts: ?Datetime.tz_options = if (tz) |t| .{ .tz = t } else null;
    return Datetime.fromUnix(nanos, .nanosecond, opts);
}

pub fn from_iso8601(string: []const u8) !Datetime {
    return Datetime.fromISO8601(string);
}

pub fn parse(string: []const u8, directives: []const u8) !Datetime {
    return Datetime.fromString(string, directives);
}

pub fn format_to(dt: Datetime, directives: []const u8, writer: *std.Io.Writer) !void {
    return dt.toString(directives, writer);
}

pub fn format_alloc(allocator: std.mem.Allocator, dt: Datetime, directives: []const u8) ![]u8 {
    var aw: std.Io.Writer.Allocating = .init(allocator);
    errdefer aw.deinit();
    try dt.toString(directives, &aw.writer);
    return aw.toOwnedSlice();
}

pub fn duration_seconds(seconds: i64) Duration {
    return Duration.fromTimespanMultiple(seconds, .second);
}

pub fn duration_minutes(minutes: i64) Duration {
    return Duration.fromTimespanMultiple(minutes, .minute);
}

pub fn duration_hours(hours: i64) Duration {
    return Duration.fromTimespanMultiple(hours, .hour);
}

pub fn duration_days(days: i64) Duration {
    return Duration.fromTimespanMultiple(days, .day);
}

pub fn add(dt: Datetime, d: Duration) !Datetime {
    return dt.add(d);
}

pub fn sub(dt: Datetime, d: Duration) !Datetime {
    return dt.sub(d);
}

pub fn diff(a: Datetime, b: Datetime) Duration {
    return a.diff(b);
}

test "utc round trip" {
    const ts: i64 = 1_700_000_000;
    const dt = try from_unix_seconds(ts, &utc);
    try std.testing.expectEqual(@as(i128, ts), dt.toUnix(.second));
}
