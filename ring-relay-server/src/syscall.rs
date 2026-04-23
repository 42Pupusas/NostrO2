//! Raw Linux x86_64 syscalls for kTLS setup.
//!
//! Only the handful we need for blocking pre-kTLS handshake I/O and for the
//! `TCP_ULP` + `TLS_TX`/`TLS_RX` setsockopts. The io_uring-driven data path
//! doesn't come through here.

use std::arch::asm;

const SYS_READ: usize = 0;
const SYS_WRITE: usize = 1;
const SYS_SETSOCKOPT: usize = 54;

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

pub unsafe fn read(fd: i32, buf: *mut u8, len: usize) -> Result<usize, std::io::Error> {
    to_io_result(unsafe { syscall3(SYS_READ, fd as usize, buf as usize, len) })
}

pub unsafe fn write(fd: i32, buf: *const u8, len: usize) -> Result<usize, std::io::Error> {
    to_io_result(unsafe { syscall3(SYS_WRITE, fd as usize, buf as usize, len) })
}

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
