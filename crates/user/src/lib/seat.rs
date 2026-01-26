use crate::sys;

pub fn create() -> sys::Result<usize> {
    sys::seatcreate()
}

pub fn destroy(seat_id: usize) -> sys::Result<()> {
    sys::seatdestroy(seat_id)
}

pub fn bind(seat_id: usize) -> sys::Result<()> {
    sys::seatbind(seat_id)
}

pub fn read_output(seat_id: usize, buf: &mut [u8]) -> sys::Result<usize> {
    sys::seatreadoutput(seat_id, buf)
}

pub fn write_input(seat_id: usize, buf: &[u8]) -> sys::Result<usize> {
    sys::seatwriteinput(seat_id, buf)
}
