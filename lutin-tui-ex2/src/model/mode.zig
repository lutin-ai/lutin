pub const Mode = enum {
    normal,
    insert,
    command,

    pub fn label(self: Mode) []const u8 {
        return switch (self) {
            .normal => " NORMAL ",
            .insert => " INSERT ",
            .command => " COMMAND ",
        };
    }
};

pub const Leader = enum {
    none,
    space,
    goto,
    space_t,
    z,
};
