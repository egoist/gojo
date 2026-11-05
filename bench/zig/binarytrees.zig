const std = @import("std");
const common = @import("common.zig");
const gpa = std.heap.c_allocator;

const Node = struct { left: ?*Node = null, right: ?*Node = null };

fn make(d: i64) !*Node {
    const n = try gpa.create(Node);
    if (d == 0) {
        n.* = .{};
    } else {
        n.* = .{ .left = try make(d - 1), .right = try make(d - 1) };
    }
    return n;
}

fn check(n: *const Node) i64 {
    var c: i64 = 1;
    if (n.left) |l| c += check(l);
    if (n.right) |r| c += check(r);
    return c;
}

fn free(n: *Node) void {
    if (n.left) |l| free(l);
    if (n.right) |r| free(r);
    gpa.destroy(n);
}

pub fn main(init: std.process.Init) !void {
    var o: common.Out = .{};
    const out = o.init(init.io);
    const n: i64 = 18;
    const min_depth: i64 = 4;
    const max_depth = @max(min_depth + 2, n);
    const stretch = max_depth + 1;
    const st = try make(stretch);
    try out.print("stretch tree of depth {d} check: {d}\n", .{ stretch, check(st) });
    free(st);
    const long = try make(max_depth);
    var d = min_depth;
    while (d <= max_depth) : (d += 2) {
        const iterations = @as(i64, 1) << @intCast(max_depth - d + min_depth);
        var sum: i64 = 0;
        var i: i64 = 0;
        while (i < iterations) : (i += 1) {
            const t = try make(d);
            sum += check(t);
            free(t);
        }
        try out.print("{d} trees of depth {d} check: {d}\n", .{ iterations, d, sum });
    }
    try out.print("long lived tree of depth {d} check: {d}\n", .{ max_depth, check(long) });
    free(long);
    try out.flush();
}
