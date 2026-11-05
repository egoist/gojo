const std = @import("std");

pub const Out = struct {
    buf: [4096]u8 = undefined,
    w: std.Io.File.Writer = undefined,

    pub fn init(self: *Out, io: std.Io) *std.Io.Writer {
        self.w = std.Io.File.stdout().writer(io, &self.buf);
        return &self.w.interface;
    }
};
