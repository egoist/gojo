const std = @import("std");
const common = @import("common.zig");

pub fn main(init: std.process.Init) !void {
    var o: common.Out = .{};
    const out = o.init(init.io);
    const n: usize = 2000;
    const fnn: f64 = @floatFromInt(n);
    var inside: i64 = 0;
    for (0..n) |y| {
        const ci = 2.0 * @as(f64, @floatFromInt(y)) / fnn - 1.0;
        for (0..n) |x| {
            const cr = 2.0 * @as(f64, @floatFromInt(x)) / fnn - 1.5;
            var zr: f64 = 0;
            var zi: f64 = 0;
            var escaped = false;
            for (0..50) |_| {
                const tr = zr * zr - zi * zi + cr;
                zi = 2.0 * zr * zi + ci;
                zr = tr;
                if (zr * zr + zi * zi > 4.0) {
                    escaped = true;
                    break;
                }
            }
            if (!escaped) inside += 1;
        }
    }
    try out.print("{d}\n", .{inside});
    try out.flush();
}
