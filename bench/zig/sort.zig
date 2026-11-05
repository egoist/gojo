const std = @import("std");
const common = @import("common.zig");

pub fn main(init: std.process.Init) !void {
    var o: common.Out = .{};
    const out = o.init(init.io);
    const gpa = std.heap.c_allocator;
    const n: usize = 5000000;
    var v: std.ArrayList(i64) = try .initCapacity(gpa, n);
    defer v.deinit(gpa);
    var seed: i64 = 42;
    for (0..n) |_| {
        seed = @rem(seed *% 1103515245 +% 12345, 2147483648);
        v.appendAssumeCapacity(seed);
    }
    std.mem.sort(i64, v.items, {}, std.sort.asc(i64));
    var ok = true;
    for (1..n) |i| {
        if (v.items[i - 1] > v.items[i]) ok = false;
    }
    try out.print("{} {d} {d} {d}\n", .{ ok, v.items[0], v.items[n / 2], v.items[n - 1] });
    try out.flush();
}
