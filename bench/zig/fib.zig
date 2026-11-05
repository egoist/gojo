const std = @import("std");
const common = @import("common.zig");

fn fib(n: i64) i64 {
    if (n < 2) return n;
    return fib(n - 1) + fib(n - 2);
}

pub fn main(init: std.process.Init) !void {
    var o: common.Out = .{};
    const out = o.init(init.io);
    try out.print("{d}\n", .{fib(40)});
    try out.flush();
}
