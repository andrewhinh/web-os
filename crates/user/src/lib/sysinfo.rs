use crate::sys;

pub fn get_nprocs() -> usize {
    sys::getnprocs().unwrap_or(1)
}

pub fn get_nprocs_conf() -> usize {
    sys::getnprocsconf().unwrap_or_else(|_| get_nprocs())
}
