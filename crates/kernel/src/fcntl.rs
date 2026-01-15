pub mod omode {
    pub const RDONLY: usize = 0x000;
    pub const WRONLY: usize = 0x001;
    pub const RDWR: usize = 0x002;
    pub const CREATE: usize = 0x200;
    pub const TRUNC: usize = 0x400;
    pub const APPEND: usize = 0x800;
    pub const CLOEXEC: usize = 0x1000;
    pub const NONBLOCK: usize = 0x2000;
}

pub mod fd {
    pub const CLOEXEC: usize = 0x1;
}

pub struct OMode {
    read: bool,
    write: bool,
    truncate: bool,
    create: bool,
    append: bool,
    cloexec: bool,
    nonblock: bool,
}

impl Default for OMode {
    fn default() -> Self {
        Self::new()
    }
}

impl OMode {
    pub fn new() -> Self {
        Self {
            read: false,
            write: false,
            truncate: false,
            create: false,
            append: false,
            cloexec: false,
            nonblock: false,
        }
    }

    pub fn read(&mut self, read: bool) -> &mut Self {
        self.read = read;
        self
    }

    pub fn write(&mut self, write: bool) -> &mut Self {
        self.write = write;
        self
    }

    pub fn append(&mut self, append: bool) -> &mut Self {
        self.append = append;
        self
    }

    pub fn cloexec(&mut self, cloexec: bool) -> &mut Self {
        self.cloexec = cloexec;
        self
    }

    pub fn nonblock(&mut self, nonblock: bool) -> &mut Self {
        self.nonblock = nonblock;
        self
    }

    fn truncate(&mut self, truncate: bool) -> &mut Self {
        self.truncate = truncate;
        self
    }

    fn create(&mut self, create: bool) -> &mut Self {
        self.create = create;
        self
    }

    pub fn from_usize(bits: usize) -> Self {
        let mut mode = Self::new();
        mode.read(bits & omode::WRONLY == 0)
            .write(bits & omode::WRONLY != 0 || bits & omode::RDWR != 0)
            .create(bits & omode::CREATE != 0)
            .truncate(bits & omode::TRUNC != 0)
            .append(bits & omode::APPEND != 0)
            .cloexec(bits & omode::CLOEXEC != 0)
            .nonblock(bits & omode::NONBLOCK != 0);
        mode
    }

    pub fn is_read(&self) -> bool {
        self.read
    }

    pub fn is_write(&self) -> bool {
        self.write
    }

    pub fn is_create(&self) -> bool {
        self.create
    }

    pub fn is_trunc(&self) -> bool {
        self.truncate
    }

    pub fn is_rdonly(&self) -> bool {
        self.read && !self.write
    }

    pub fn is_cloexec(&self) -> bool {
        self.cloexec
    }

    pub fn is_append(&self) -> bool {
        self.append
    }

    pub fn is_nonblock(&self) -> bool {
        self.nonblock
    }
}

#[repr(usize)]
pub enum FcntlCmd {
    GetFl = 1,
    SetFl = 2,
    GetFd = 3,
    SetFd = 4,
    SetCloexec = 5,
    SetNonblock = 6,
    ClearNonblock = 7,
    Invalid,
}

impl FcntlCmd {
    pub fn from_usize(bits: usize) -> Self {
        match bits {
            1 => Self::GetFl,
            2 => Self::SetFl,
            3 => Self::GetFd,
            4 => Self::SetFd,
            5 => Self::SetCloexec,
            6 => Self::SetNonblock,
            7 => Self::ClearNonblock,
            _ => Self::Invalid,
        }
    }
}
