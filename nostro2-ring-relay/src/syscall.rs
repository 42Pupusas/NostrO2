//! Raw Linux x86_64 syscalls — replaces libc for the handful of calls we need.

use std::arch::asm;

// Syscall numbers (x86_64)
const SYS_WRITE: usize = 1;
const SYS_CLOSE: usize = 3;
const SYS_RECVMSG: usize = 47;
const SYS_SHUTDOWN: usize = 48;
const SYS_SETSOCKOPT: usize = 54;

#[inline(always)]
unsafe fn syscall2(nr: usize, a1: usize, a2: usize) -> isize {
    let ret: isize;
    unsafe {
        asm!(
            "syscall",
            inlateout("rax") nr as isize => ret,
            in("rdi") a1,
            in("rsi") a2,
            lateout("rcx") _,
            lateout("r11") _,
            options(nostack),
        );
    }
    ret
}

#[inline(always)]
unsafe fn syscall3(nr: usize, a1: usize, a2: usize, a3: usize) -> isize {
    let ret: isize;
    unsafe {
        asm!(
            "syscall",
            inlateout("rax") nr as isize => ret,
            in("rdi") a1,
            in("rsi") a2,
            in("rdx") a3,
            lateout("rcx") _,
            lateout("r11") _,
            options(nostack),
        );
    }
    ret
}

#[inline(always)]
unsafe fn syscall5(nr: usize, a1: usize, a2: usize, a3: usize, a4: usize, a5: usize) -> isize {
    let ret: isize;
    unsafe {
        asm!(
            "syscall",
            inlateout("rax") nr as isize => ret,
            in("rdi") a1,
            in("rsi") a2,
            in("rdx") a3,
            in("r10") a4,
            in("r8") a5,
            lateout("rcx") _,
            lateout("r11") _,
            options(nostack),
        );
    }
    ret
}

fn to_io_result(ret: isize) -> Result<usize, std::io::Error> {
    if ret < 0 {
        Err(std::io::Error::from_raw_os_error(-ret as i32))
    } else {
        Ok(ret as usize)
    }
}

// ── Public syscall wrappers ──────────────────────────────────────────

pub unsafe fn write(fd: i32, buf: *const u8, len: usize) -> Result<usize, std::io::Error> {
    to_io_result(unsafe { syscall3(SYS_WRITE, fd as usize, buf as usize, len) })
}

pub unsafe fn close(fd: i32) {
    unsafe {
        syscall2(SYS_CLOSE, fd as usize, 0);
    }
}

pub unsafe fn shutdown(fd: i32, how: i32) {
    unsafe {
        syscall2(SYS_SHUTDOWN, fd as usize, how as usize);
    }
}

pub const SHUT_RDWR: i32 = 2;

pub unsafe fn setsockopt(
    fd: i32,
    level: i32,
    optname: i32,
    optval: *const u8,
    optlen: u32,
) -> Result<(), std::io::Error> {
    let ret = unsafe {
        syscall5(
            SYS_SETSOCKOPT,
            fd as usize,
            level as usize,
            optname as usize,
            optval as usize,
            optlen as usize,
        )
    };
    if ret < 0 {
        Err(std::io::Error::from_raw_os_error(-ret as i32))
    } else {
        Ok(())
    }
}

pub unsafe fn recvmsg(fd: i32, msg: *mut MsgHdr, flags: i32) -> Result<usize, std::io::Error> {
    to_io_result(unsafe { syscall3(SYS_RECVMSG, fd as usize, msg as usize, flags as usize) })
}

// ── Types ────────────────────────────────────────────────────────────

#[repr(C)]
pub struct IoVec {
    pub iov_base: *mut u8,
    pub iov_len: usize,
}

#[repr(C)]
pub struct MsgHdr {
    pub msg_name: *mut u8,
    pub msg_namelen: u32,
    _pad1: u32,
    pub msg_iov: *mut IoVec,
    pub msg_iovlen: usize,
    pub msg_control: *mut u8,
    pub msg_controllen: usize,
    pub msg_flags: i32,
    _pad2: u32,
}

impl MsgHdr {
    pub fn zeroed() -> Self {
        unsafe { std::mem::zeroed() }
    }
}

#[repr(C)]
pub struct CmsgHdr {
    pub cmsg_len: usize,
    pub cmsg_level: i32,
    pub cmsg_type: i32,
}

// ── CMSG helpers (matching kernel behavior on x86_64) ────────────────

const CMSG_ALIGN: usize = std::mem::size_of::<usize>(); // 8 on x86_64

const fn cmsg_align(len: usize) -> usize {
    (len + CMSG_ALIGN - 1) & !(CMSG_ALIGN - 1)
}

/// Returns a pointer to the first `CmsgHdr` in the message, or null.
pub unsafe fn cmsg_firsthdr(msg: &MsgHdr) -> *mut CmsgHdr {
    if msg.msg_controllen >= std::mem::size_of::<CmsgHdr>() {
        msg.msg_control as *mut CmsgHdr
    } else {
        std::ptr::null_mut()
    }
}

/// Returns a pointer to the next `CmsgHdr` after `cmsg`, or null.
pub unsafe fn cmsg_nxthdr(msg: &MsgHdr, cmsg: *const CmsgHdr) -> *mut CmsgHdr {
    let next = (cmsg as usize) + cmsg_align(unsafe { (*cmsg).cmsg_len });
    let end = msg.msg_control as usize + msg.msg_controllen;
    if next + std::mem::size_of::<CmsgHdr>() <= end {
        next as *mut CmsgHdr
    } else {
        std::ptr::null_mut()
    }
}

/// Returns a pointer to the data portion of a `CmsgHdr`.
pub unsafe fn cmsg_data(cmsg: *const CmsgHdr) -> *const u8 {
    unsafe { (cmsg as *const u8).add(cmsg_align(std::mem::size_of::<CmsgHdr>())) }
}
