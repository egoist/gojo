const std = @import("std");
const common = @import("common.zig");
const gpa = std.heap.c_allocator;

pub fn main(init: std.process.Init) !void {
    var o: common.Out = .{};
    const out = o.init(init.io);
    const n: i64 = 1000000;
    var m: std.AutoHashMapUnmanaged(i64, i64) = .empty;
    defer m.deinit(gpa);
    var i: i64 = 0;
    while (i < n) : (i += 1) try m.put(gpa, @rem(i * 7919, 2000003), i);
    var sum: i64 = 0;
    i = 0;
    while (i < n) : (i += 1) {
        if (m.get(i)) |v| sum += v;
    }
    try out.print("{d} {d}\n", .{ m.count(), sum });

    const k: i64 = 200000;
    var s: std.StringHashMapUnmanaged(i64) = .empty;
    i = 0;
    while (i < k) : (i += 1) {
        const key = try std.fmt.allocPrint(gpa, "{d}", .{i});
        const gop = try s.getOrPut(gpa, key);
        if (gop.found_existing) gpa.free(key);
        gop.value_ptr.* = i;
    }
    var hits: i64 = 0;
    i = 0;
    while (i < k * 2) : (i += 1) {
        // allocate the probe key, like the Go and Kite versions do
        const key = try std.fmt.allocPrint(gpa, "{d}", .{i});
        if (s.contains(key)) hits += 1;
        gpa.free(key);
    }
    try out.print("{d} {d}\n", .{ s.count(), hits });
    var it = s.keyIterator();
    while (it.next()) |key| gpa.free(key.*);
    s.deinit(gpa);
    try out.flush();
}
