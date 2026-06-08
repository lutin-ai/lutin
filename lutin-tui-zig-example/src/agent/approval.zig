const std = @import("std");
const Io = std.Io;
const Map = std.process.Environ.Map;

const paths = @import("../config/paths.zig");
const jsonfile = @import("../config/jsonfile.zig");

pub const Decision = enum { allow, deny, ask };

pub const Rule = struct {
    tool: []const u8,
    decision: Decision,
};

pub const PersonaPolicy = struct {
    persona: []const u8,
    rules: []Rule,
};

const OnDisk = struct {
    personas: []PersonaPolicy,
};

fn approvalsPath(io: Io, alloc: std.mem.Allocator, env: *const Map) ![]u8 {
    const cfg = try paths.configDir(io, alloc, env);
    defer alloc.free(cfg);
    return std.fs.path.join(alloc, &.{ cfg, "approvals.json" });
}

pub const Policy = struct {
    personas: []PersonaPolicy,
    alloc: std.mem.Allocator,

    pub fn empty(alloc: std.mem.Allocator) Policy {
        return .{ .personas = &.{}, .alloc = alloc };
    }

    pub fn loadOrEmpty(alloc: std.mem.Allocator, io: Io, env: *const Map) !Policy {
        const path = try approvalsPath(io, alloc, env);
        defer alloc.free(path);

        const bytes = jsonfile.readAlloc(io, alloc, path) catch |err| switch (err) {
            error.FileNotFound => return Policy.empty(alloc),
            else => return err,
        };
        defer alloc.free(bytes);

        var parsed = try jsonfile.parseJson(OnDisk, alloc, bytes);
        defer parsed.deinit();

        return cloneFromOnDisk(alloc, parsed.value);
    }

    pub fn save(self: *const Policy, io: Io, env: *const Map) !void {
        const path = try approvalsPath(io, self.alloc, env);
        defer self.alloc.free(path);

        const on_disk: OnDisk = .{ .personas = self.personas };
        const bytes = try jsonfile.stringifyAlloc(self.alloc, on_disk);
        defer self.alloc.free(bytes);

        try jsonfile.writeAtomic(io, self.alloc, path, bytes);
    }

    pub fn deinit(self: *Policy) void {
        freePersonas(self.alloc, self.personas);
        self.personas = &.{};
    }

    pub fn decide(self: *const Policy, persona: []const u8, tool: []const u8) Decision {
        for (self.personas) |pp| {
            if (!std.mem.eql(u8, pp.persona, persona)) continue;
            for (pp.rules) |r| {
                if (std.mem.eql(u8, r.tool, "*") or std.mem.eql(u8, r.tool, tool)) {
                    return r.decision;
                }
            }
        }
        return .ask;
    }

    pub fn remember(self: *Policy, persona: []const u8, tool: []const u8, decision: Decision) !void {
        if (decision == .ask) return error.InvalidDecision;

        for (self.personas, 0..) |pp, pi| {
            if (!std.mem.eql(u8, pp.persona, persona)) continue;

            for (pp.rules, 0..) |r, ri| {
                if (std.mem.eql(u8, r.tool, tool)) {
                    self.personas[pi].rules[ri].decision = decision;
                    return;
                }
            }

            const new_rules = try self.alloc.alloc(Rule, pp.rules.len + 1);
            @memcpy(new_rules[0..pp.rules.len], pp.rules);
            new_rules[pp.rules.len] = .{
                .tool = try self.alloc.dupe(u8, tool),
                .decision = decision,
            };
            self.alloc.free(pp.rules);
            self.personas[pi].rules = new_rules;
            return;
        }

        const new_personas = try self.alloc.alloc(PersonaPolicy, self.personas.len + 1);
        @memcpy(new_personas[0..self.personas.len], self.personas);

        const rules = try self.alloc.alloc(Rule, 1);
        rules[0] = .{ .tool = try self.alloc.dupe(u8, tool), .decision = decision };
        new_personas[self.personas.len] = .{
            .persona = try self.alloc.dupe(u8, persona),
            .rules = rules,
        };
        self.alloc.free(self.personas);
        self.personas = new_personas;
    }
};

fn cloneFromOnDisk(alloc: std.mem.Allocator, src: OnDisk) !Policy {
    const personas = try alloc.alloc(PersonaPolicy, src.personas.len);
    errdefer alloc.free(personas);

    var filled: usize = 0;
    errdefer {
        for (personas[0..filled]) |pp| {
            for (pp.rules) |r| alloc.free(r.tool);
            alloc.free(pp.rules);
            alloc.free(pp.persona);
        }
    }

    for (src.personas, 0..) |pp, i| {
        const rules = try alloc.alloc(Rule, pp.rules.len);
        var rfilled: usize = 0;
        errdefer {
            for (rules[0..rfilled]) |r| alloc.free(r.tool);
            alloc.free(rules);
        }
        for (pp.rules, 0..) |r, j| {
            rules[j] = .{ .tool = try alloc.dupe(u8, r.tool), .decision = r.decision };
            rfilled = j + 1;
        }
        personas[i] = .{ .persona = try alloc.dupe(u8, pp.persona), .rules = rules };
        filled = i + 1;
    }

    return .{ .personas = personas, .alloc = alloc };
}

fn freePersonas(alloc: std.mem.Allocator, personas: []PersonaPolicy) void {
    for (personas) |pp| {
        for (pp.rules) |r| alloc.free(r.tool);
        alloc.free(pp.rules);
        alloc.free(pp.persona);
    }
    alloc.free(personas);
}

test "decide returns ask when no rule matches" {
    var p = Policy.empty(std.testing.allocator);
    defer p.deinit();
    try std.testing.expectEqual(Decision.ask, p.decide("alice", "fs.read"));
}

test "remember then decide round-trips a rule" {
    var p = Policy.empty(std.testing.allocator);
    defer p.deinit();
    try p.remember("alice", "fs.read", .allow);
    try std.testing.expectEqual(Decision.allow, p.decide("alice", "fs.read"));
    try std.testing.expectEqual(Decision.ask, p.decide("alice", "fs.write"));
    try std.testing.expectEqual(Decision.ask, p.decide("bob", "fs.read"));
}

test "remember replaces existing rule for same (persona, tool)" {
    var p = Policy.empty(std.testing.allocator);
    defer p.deinit();
    try p.remember("alice", "fs.read", .allow);
    try p.remember("alice", "fs.read", .deny);
    try std.testing.expectEqual(Decision.deny, p.decide("alice", "fs.read"));
}

test "remember with ask returns InvalidDecision" {
    var p = Policy.empty(std.testing.allocator);
    defer p.deinit();
    try std.testing.expectError(error.InvalidDecision, p.remember("alice", "fs.read", .ask));
}

test "json round-trip preserves shape" {
    const alloc = std.testing.allocator;
    var p = Policy.empty(alloc);
    defer p.deinit();
    try p.remember("alice", "fs.read", .allow);
    try p.remember("alice", "net.fetch", .deny);
    try p.remember("bob", "*", .allow);

    const on_disk: OnDisk = .{ .personas = p.personas };
    const bytes = try jsonfile.stringifyAlloc(alloc, on_disk);
    defer alloc.free(bytes);

    var parsed = try jsonfile.parseJson(OnDisk, alloc, bytes);
    defer parsed.deinit();

    var p2 = try cloneFromOnDisk(alloc, parsed.value);
    defer p2.deinit();

    try std.testing.expectEqual(Decision.allow, p2.decide("alice", "fs.read"));
    try std.testing.expectEqual(Decision.deny, p2.decide("alice", "net.fetch"));
    try std.testing.expectEqual(Decision.allow, p2.decide("bob", "whatever"));
}

test "wildcard rule matches any tool" {
    var p = Policy.empty(std.testing.allocator);
    defer p.deinit();
    try p.remember("alice", "*", .deny);
    try std.testing.expectEqual(Decision.deny, p.decide("alice", "anything"));
}
