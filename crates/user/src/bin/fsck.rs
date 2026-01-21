#![no_std]
extern crate alloc;

use alloc::vec::Vec;
use core::mem::size_of;

use kernel::fs::{BPB, BSIZE, DirEnt, FSMAGIC, IPB, NDIRECT, NINDIRECT, ROOTINO, SuperBlock};
use kernel::stat::FileType;
use ulib::{eprintln, fs::File, io::Read, println, sys};

const INVALID_U32: u32 = u32::MAX;

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct DInode {
    itype: u16,
    major: u16,
    minor: u16,
    nlink: u16,
    size: u32,
    addrs: [u32; NDIRECT + 2],
    atime: u64,
    mtime: u64,
    ctime: u64,
    _padding: [u32; 10],
}

fn fail(msg: &str) -> ! {
    eprintln!("{}", msg);
    sys::exit(1)
}

fn open_disk() -> File {
    match File::open("/dev/disk") {
        Ok(f) => f,
        Err(_) => fail("fsck: open /dev/disk"),
    }
}

fn read_block(disk: &mut File, buf: &mut [u8; BSIZE]) {
    match disk.read(buf) {
        Ok(n) if n == BSIZE => {}
        Ok(_) => fail("fsck: short read"),
        Err(_) => fail("fsck: read err"),
    }
}

fn parse_superblock(buf: &[u8; BSIZE]) -> SuperBlock {
    let raw = unsafe { (buf.as_ptr() as *const SuperBlock).read_unaligned() };
    SuperBlock {
        magic: u32::from_le(raw.magic),
        size: u32::from_le(raw.size),
        nblocks: u32::from_le(raw.nblocks),
        ninodes: u32::from_le(raw.ninodes),
        nlog: u32::from_le(raw.nlog),
        logstart: u32::from_le(raw.logstart),
        inodestart: u32::from_le(raw.inodestart),
        bmapstart: u32::from_le(raw.bmapstart),
    }
}

fn inode_at(buf: &[u8; BSIZE], idx: usize) -> DInode {
    let offset = idx * size_of::<DInode>();
    let ptr = unsafe { buf.as_ptr().add(offset) as *const DInode };
    unsafe { ptr.read_unaligned() }
}

fn dirent_at(buf: &[u8; BSIZE], idx: usize) -> DirEnt {
    let offset = idx * size_of::<DirEnt>();
    let ptr = unsafe { buf.as_ptr().add(offset) as *const DirEnt };
    unsafe { ptr.read_unaligned() }
}

fn u32_at(buf: &[u8; BSIZE], idx: usize) -> u32 {
    let offset = idx * size_of::<u32>();
    let bytes = [
        buf[offset],
        buf[offset + 1],
        buf[offset + 2],
        buf[offset + 3],
    ];
    u32::from_le_bytes(bytes)
}

fn filetype_from_u16(bits: u16) -> Option<FileType> {
    match bits {
        0 => Some(FileType::Empty),
        1 => Some(FileType::Dir),
        2 => Some(FileType::File),
        3 => Some(FileType::Device),
        4 => Some(FileType::Symlink),
        5 => Some(FileType::Socket),
        _ => None,
    }
}

fn vec_fill<T: Clone>(len: usize, value: T) -> Vec<T> {
    let mut v = Vec::with_capacity(len);
    v.resize(len, value);
    v
}

fn validate_addr(addr: u32, data_start: u32, sb_size: usize) {
    if addr < data_start || addr as usize >= sb_size {
        fail("fsck: bad addr");
    }
}

fn mark_block(block_refcnt: &mut [u32], addr: u32) {
    let idx = addr as usize;
    block_refcnt[idx] += 1;
    if block_refcnt[idx] > 1 {
        fail("fsck: dup block");
    }
}

fn add_data_block(
    owner: u32,
    addr: u32,
    data_start: u32,
    sb_size: usize,
    block_refcnt: &mut [u32],
    data_block_owner: &mut [u32],
    inode_data_blocks: &mut [usize],
    inode_is_dir: &[bool],
    dir_block_map: &mut [bool],
) {
    validate_addr(addr, data_start, sb_size);
    mark_block(block_refcnt, addr);
    let idx = addr as usize;
    if data_block_owner[idx] != INVALID_U32 && data_block_owner[idx] != owner {
        fail("fsck: dup block");
    }
    data_block_owner[idx] = owner;
    inode_data_blocks[owner as usize] += 1;
    if inode_is_dir[owner as usize] {
        dir_block_map[idx] = true;
    }
}

fn main() {
    println!("fsck: start");
    let mut disk = open_disk();
    let mut buf = [0u8; BSIZE];

    read_block(&mut disk, &mut buf); // boot
    read_block(&mut disk, &mut buf); // superblock

    let sb = parse_superblock(&buf);
    if sb.magic != FSMAGIC {
        fail("fsck: bad super");
    }

    let sb_size = sb.size as usize;
    let ninodes = sb.ninodes as usize;
    if sb_size == 0 || ninodes == 0 || sb.nblocks == 0 {
        fail("fsck: bad super");
    }

    let n_inode_blocks = (ninodes + IPB - 1) / IPB;
    let n_bitmap_blocks = (sb_size + BPB as usize - 1) / BPB as usize;
    let data_start = sb.bmapstart as usize + n_bitmap_blocks;

    if sb.logstart < 2
        || (sb.inodestart as usize) < sb.logstart as usize + sb.nlog as usize
        || (sb.bmapstart as usize) < sb.inodestart as usize + n_inode_blocks
        || data_start > sb_size
        || sb.nblocks as usize != sb_size - data_start
    {
        fail("fsck: bad super");
    }

    let data_start_u32 = data_start as u32;
    let mut inode_used = vec_fill(ninodes, false);
    let mut inode_type = vec_fill(ninodes, FileType::Empty);
    let mut inode_nlink = vec_fill(ninodes, 0u16);
    let mut inode_size = vec_fill(ninodes, 0u32);
    let mut inode_is_dir = vec_fill(ninodes, false);
    let mut inode_data_blocks = vec_fill(ninodes, 0usize);

    let mut block_refcnt = vec_fill(sb_size, 0u32);
    let mut data_block_owner = vec_fill(sb_size, INVALID_U32);
    let mut single_indirect_owner = vec_fill(sb_size, INVALID_U32);
    let mut double_indirect_owner = vec_fill(sb_size, INVALID_U32);
    let mut second_level_owner = vec_fill(sb_size, INVALID_U32);

    let mut bitmap_used = vec_fill(sb_size, false);
    let mut dir_block_map = vec_fill(sb_size, false);

    let mut dir_has_dot = vec_fill(ninodes, false);
    let mut dir_has_dotdot = vec_fill(ninodes, false);
    let mut dir_dot_inum = vec_fill(ninodes, 0u32);
    let mut dir_dotdot_inum = vec_fill(ninodes, 0u32);
    let mut dir_refcnt = vec_fill(ninodes, 0u32);
    let mut dir_parent_cnt = vec_fill(ninodes, 0u32);

    let inode_start = sb.inodestart as usize;
    let inode_end = inode_start + n_inode_blocks;
    let bmap_start = sb.bmapstart as usize;
    let bmap_end = bmap_start + n_bitmap_blocks;

    let mut block_no = 2usize;
    let mut scan_end = bmap_end.saturating_sub(1).max(inode_end.saturating_sub(1));
    while block_no < sb_size && block_no <= scan_end {
        read_block(&mut disk, &mut buf);
        if block_no >= inode_start && block_no < inode_end {
            let base_inum = (block_no - inode_start) * IPB;
            for idx in 0..IPB {
                let inum = base_inum + idx;
                if inum >= ninodes {
                    break;
                }
                let din = inode_at(&buf, idx);
                let itype_raw = u16::from_le(din.itype);
                let ftype = match filetype_from_u16(itype_raw) {
                    Some(t) => t,
                    None => fail("fsck: bad inode type"),
                };
                if ftype == FileType::Empty {
                    continue;
                }
                inode_used[inum] = true;
                inode_type[inum] = ftype;
                inode_nlink[inum] = u16::from_le(din.nlink);
                inode_size[inum] = u32::from_le(din.size);
                if ftype == FileType::Dir {
                    inode_is_dir[inum] = true;
                }

                for j in 0..NDIRECT {
                    let addr = u32::from_le(din.addrs[j]);
                    if addr == 0 {
                        continue;
                    }
                    add_data_block(
                        inum as u32,
                        addr,
                        data_start_u32,
                        sb_size,
                        &mut block_refcnt,
                        &mut data_block_owner,
                        &mut inode_data_blocks,
                        &inode_is_dir,
                        &mut dir_block_map,
                    );
                    if addr as usize > scan_end {
                        scan_end = addr as usize;
                    }
                }

                let ind = u32::from_le(din.addrs[NDIRECT]);
                if ind != 0 {
                    validate_addr(ind, data_start_u32, sb_size);
                    mark_block(&mut block_refcnt, ind);
                    if single_indirect_owner[ind as usize] != INVALID_U32 {
                        fail("fsck: dup block");
                    }
                    single_indirect_owner[ind as usize] = inum as u32;
                    if ind as usize > scan_end {
                        scan_end = ind as usize;
                    }
                }

                let dind = u32::from_le(din.addrs[NDIRECT + 1]);
                if dind != 0 {
                    validate_addr(dind, data_start_u32, sb_size);
                    mark_block(&mut block_refcnt, dind);
                    if double_indirect_owner[dind as usize] != INVALID_U32 {
                        fail("fsck: dup block");
                    }
                    double_indirect_owner[dind as usize] = inum as u32;
                    if dind as usize > scan_end {
                        scan_end = dind as usize;
                    }
                }
            }
        }
        if block_no >= bmap_start && block_no < bmap_end {
            let base = (block_no - sb.bmapstart as usize) * BPB as usize;
            for bit in 0..BPB as usize {
                let blk = base + bit;
                if blk >= sb_size {
                    break;
                }
                let byte = buf[bit / 8];
                if byte & (1u8 << (bit % 8)) != 0 {
                    bitmap_used[blk] = true;
                }
            }
        }

        if block_no >= data_start {
            let owner = single_indirect_owner[block_no];
            if owner != INVALID_U32 {
                for i in 0..NINDIRECT {
                    let addr = u32_at(&buf, i);
                    if addr == 0 {
                        continue;
                    }
                    add_data_block(
                        owner,
                        addr,
                        data_start_u32,
                        sb_size,
                        &mut block_refcnt,
                        &mut data_block_owner,
                        &mut inode_data_blocks,
                        &inode_is_dir,
                        &mut dir_block_map,
                    );
                    if addr as usize > scan_end {
                        scan_end = addr as usize;
                    }
                }
            }

            let owner = double_indirect_owner[block_no];
            if owner != INVALID_U32 {
                for i in 0..NINDIRECT {
                    let addr = u32_at(&buf, i);
                    if addr == 0 {
                        continue;
                    }
                    validate_addr(addr, data_start_u32, sb_size);
                    mark_block(&mut block_refcnt, addr);
                    if second_level_owner[addr as usize] != INVALID_U32 {
                        fail("fsck: dup block");
                    }
                    second_level_owner[addr as usize] = owner;
                    if addr as usize > scan_end {
                        scan_end = addr as usize;
                    }
                }
            }

            let owner = second_level_owner[block_no];
            if owner != INVALID_U32 {
                for i in 0..NINDIRECT {
                    let addr = u32_at(&buf, i);
                    if addr == 0 {
                        continue;
                    }
                    add_data_block(
                        owner,
                        addr,
                        data_start_u32,
                        sb_size,
                        &mut block_refcnt,
                        &mut data_block_owner,
                        &mut inode_data_blocks,
                        &inode_is_dir,
                        &mut dir_block_map,
                    );
                    if addr as usize > scan_end {
                        scan_end = addr as usize;
                    }
                }
            }

            if dir_block_map[block_no] {
                let dir_inum = data_block_owner[block_no] as usize;
                let dirents_per_block = BSIZE / size_of::<DirEnt>();
                for i in 0..dirents_per_block {
                    let de = dirent_at(&buf, i);
                    let inum = u16::from_le(de.inum) as u32;
                    if inum == 0 {
                        continue;
                    }
                    if inum as usize >= ninodes {
                        fail("fsck: dir entry inum");
                    }
                    if !inode_used[inum as usize] {
                        fail("fsck: dir entry free");
                    }
                    let name_len = de
                        .name
                        .iter()
                        .position(|&b| b == 0)
                        .unwrap_or(de.name.len());
                    let name = &de.name[..name_len];
                    if name == b"." {
                        dir_has_dot[dir_inum] = true;
                        dir_dot_inum[dir_inum] = inum;
                    } else if name == b".." {
                        dir_has_dotdot[dir_inum] = true;
                        dir_dotdot_inum[dir_inum] = inum;
                    } else if inode_type[inum as usize] == FileType::Dir {
                        dir_parent_cnt[inum as usize] += 1;
                        if dir_parent_cnt[inum as usize] > 1 {
                            fail("fsck: dir dup parent");
                        }
                    }
                    dir_refcnt[inum as usize] += 1;
                }
            }
        }
        block_no += 1;
    }

    for i in data_start..sb_size {
        if block_refcnt[i] > 0 && !bitmap_used[i] {
            fail("fsck: addr free in bitmap");
        }
        if bitmap_used[i] && block_refcnt[i] == 0 {
            fail("fsck: bitmap uses free");
        }
    }

    let root = ROOTINO as usize;
    if root >= ninodes || !inode_used[root] || inode_type[root] != FileType::Dir {
        fail("fsck: root bad");
    }

    for inum in 0..ninodes {
        if !inode_used[inum] {
            continue;
        }

        let blocks = inode_data_blocks[inum];
        let size = inode_size[inum] as usize;
        if blocks == 0 {
            if size != 0 {
                fail("fsck: bad size");
            }
        } else if size == 0 || size > blocks * BSIZE || size <= (blocks - 1) * BSIZE {
            fail("fsck: bad size");
        }

        if inode_type[inum] == FileType::Dir {
            if !dir_has_dot[inum] || !dir_has_dotdot[inum] || dir_dot_inum[inum] != inum as u32 {
                fail("fsck: dir fmt");
            }
            if inum == root && dir_dotdot_inum[inum] != ROOTINO {
                fail("fsck: dir fmt");
            }
        } else if inode_nlink[inum] != dir_refcnt[inum] as u16 {
            fail("fsck: bad nlink");
        }

        if inum != root && dir_refcnt[inum] == 0 {
            fail("fsck: inode unref");
        }
    }

    println!("fsck: ok");
}
