const std = @import("std");
const common = @import("common.zig");

pub fn main(init: std.process.Init) !void {
    var o: common.Out = .{};
    const out = o.init(init.io);
    const n: usize = 10;
    var perm: [n]usize = undefined;
    var perm1: [n]usize = undefined;
    var count: [n]usize = undefined;
    for (&perm1, 0..) |*p, i| p.* = i;
    var max_flips: i64 = 0;
    var checksum: i64 = 0;
    var perm_count: i64 = 0;
    var r: usize = n;
    outer: while (true) {
        while (r != 1) : (r -= 1) count[r - 1] = r;
        perm = perm1;
        var flips: i64 = 0;
        var k = perm[0];
        while (k != 0) : (k = perm[0]) {
            var lo: usize = 0;
            var hi: usize = k;
            while (lo < hi) : ({
                lo += 1;
                hi -= 1;
            }) {
                const t = perm[lo];
                perm[lo] = perm[hi];
                perm[hi] = t;
            }
            flips += 1;
        }
        if (flips > max_flips) max_flips = flips;
        if (@rem(perm_count, 2) == 0) checksum += flips else checksum -= flips;
        while (true) {
            if (r == n) break :outer;
            const p0 = perm1[0];
            for (0..r) |i| perm1[i] = perm1[i + 1];
            perm1[r] = p0;
            count[r] -= 1;
            if (count[r] > 0) break;
            r += 1;
        }
        perm_count += 1;
    }
    try out.print("{d}\nPfannkuchen( {d} ) = {d}\n", .{ checksum, n, max_flips });
    try out.flush();
}
