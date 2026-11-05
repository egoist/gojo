const std = @import("std");
const common = @import("common.zig");

fn a(i: usize, j: usize) f64 {
    return 1.0 / @as(f64, @floatFromInt((i + j) * (i + j + 1) / 2 + i + 1));
}

fn mulAv(v: []const f64, out: []f64) void {
    for (out, 0..) |*o, i| {
        var s: f64 = 0;
        for (v, 0..) |x, j| s += a(i, j) * x;
        o.* = s;
    }
}

fn mulAtv(v: []const f64, out: []f64) void {
    for (out, 0..) |*o, i| {
        var s: f64 = 0;
        for (v, 0..) |x, j| s += a(j, i) * x;
        o.* = s;
    }
}

fn mulAtAv(v: []const f64, out: []f64, tmp: []f64) void {
    mulAv(v, tmp);
    mulAtv(tmp, out);
}

pub fn main(init: std.process.Init) !void {
    var o: common.Out = .{};
    const out = o.init(init.io);
    const gpa = std.heap.c_allocator;
    const n = 2000;
    const u = try gpa.alloc(f64, n);
    const v = try gpa.alloc(f64, n);
    const tmp = try gpa.alloc(f64, n);
    @memset(u, 1);
    @memset(v, 0);
    for (0..10) |_| {
        mulAtAv(u, v, tmp);
        mulAtAv(v, u, tmp);
    }
    var vBv: f64 = 0;
    var vv: f64 = 0;
    for (0..n) |i| {
        vBv += u[i] * v[i];
        vv += v[i] * v[i];
    }
    try out.print("{d}\n", .{@as(i64, @intFromFloat(@round(@sqrt(vBv / vv) * 1e9)))});
    try out.flush();
}
