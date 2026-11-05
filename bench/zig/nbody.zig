const std = @import("std");
const common = @import("common.zig");

const Body = struct { x: f64, y: f64, z: f64, vx: f64, vy: f64, vz: f64, mass: f64 };

fn advance(bodies: []Body, dt: f64) void {
    const n = bodies.len;
    for (0..n) |i| {
        for (i + 1..n) |j| {
            const dx = bodies[i].x - bodies[j].x;
            const dy = bodies[i].y - bodies[j].y;
            const dz = bodies[i].z - bodies[j].z;
            const d2 = dx * dx + dy * dy + dz * dz;
            const mag = dt / (d2 * @sqrt(d2));
            const mi = bodies[i].mass * mag;
            const mj = bodies[j].mass * mag;
            bodies[i].vx -= dx * mj;
            bodies[i].vy -= dy * mj;
            bodies[i].vz -= dz * mj;
            bodies[j].vx += dx * mi;
            bodies[j].vy += dy * mi;
            bodies[j].vz += dz * mi;
        }
    }
    for (bodies) |*b| {
        b.x += dt * b.vx;
        b.y += dt * b.vy;
        b.z += dt * b.vz;
    }
}

fn energy(bodies: []const Body) f64 {
    var e: f64 = 0;
    for (bodies, 0..) |b, i| {
        e += 0.5 * b.mass * (b.vx * b.vx + b.vy * b.vy + b.vz * b.vz);
        for (bodies[i + 1 ..]) |c| {
            const dx = b.x - c.x;
            const dy = b.y - c.y;
            const dz = b.z - c.z;
            e -= b.mass * c.mass / @sqrt(dx * dx + dy * dy + dz * dz);
        }
    }
    return e;
}

pub fn main(init: std.process.Init) !void {
    var o: common.Out = .{};
    const out = o.init(init.io);
    const pi = 3.141592653589793;
    const solar_mass = 4.0 * pi * pi;
    const days = 365.24;
    var bodies = [_]Body{
        .{ .x = 0, .y = 0, .z = 0, .vx = 0, .vy = 0, .vz = 0, .mass = solar_mass },
        .{ .x = 4.84143144246472090e+00, .y = -1.16032004402742839e+00, .z = -1.03622044471123109e-01, .vx = 1.66007664274403694e-03 * days, .vy = 7.69901118419740425e-03 * days, .vz = -6.90460016972063023e-05 * days, .mass = 9.54791938424326609e-04 * solar_mass },
        .{ .x = 8.34336671824457987e+00, .y = 4.12479856412430479e+00, .z = -4.03523417114321381e-01, .vx = -2.76742510726862411e-03 * days, .vy = 4.99852801234917238e-03 * days, .vz = 2.30417297573763929e-05 * days, .mass = 2.85885980666130812e-04 * solar_mass },
        .{ .x = 1.28943695621391310e+01, .y = -1.51111514016986312e+01, .z = -2.23307578892655734e-01, .vx = 2.96460137564761618e-03 * days, .vy = 2.37847173959480950e-03 * days, .vz = -2.96589568540237556e-05 * days, .mass = 4.36624404335156298e-05 * solar_mass },
        .{ .x = 1.53796971148509165e+01, .y = -2.59193146099879641e+01, .z = 1.79258772950371181e-01, .vx = 2.68067772490389322e-03 * days, .vy = 1.62824170038242295e-03 * days, .vz = -9.51592254519715870e-05 * days, .mass = 5.15138902046611451e-05 * solar_mass },
    };
    var px: f64 = 0;
    var py: f64 = 0;
    var pz: f64 = 0;
    for (bodies) |b| {
        px += b.vx * b.mass;
        py += b.vy * b.mass;
        pz += b.vz * b.mass;
    }
    bodies[0].vx = -px / solar_mass;
    bodies[0].vy = -py / solar_mass;
    bodies[0].vz = -pz / solar_mass;
    try out.print("{d}\n", .{@as(i64, @intFromFloat(@round(energy(&bodies) * 1e9)))});
    for (0..5000000) |_| advance(&bodies, 0.01);
    try out.print("{d}\n", .{@as(i64, @intFromFloat(@round(energy(&bodies) * 1e9)))});
    try out.flush();
}
