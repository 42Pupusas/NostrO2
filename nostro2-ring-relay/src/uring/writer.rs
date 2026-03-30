//! Writer thread: broadcast ring pop -> WebSocket frame encode -> send.

use quetzalcoatl::broadcast;
use quetzalcoatl::mpsc::Consumer;
use rand::Rng;
use std::os::unix::io::RawFd;
use std::sync::atomic::{AtomicBool, Ordering};

use super::ws;

/// Run the writer loop. Blocks until shutdown.
pub fn writer_loop(
    fd: RawFd,
    mut outbound: broadcast::Consumer<String>,
    shutdown: &AtomicBool,
    mut pong_rx: Consumer<Vec<u8>>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let mut frame_buf = Vec::with_capacity(65536);
    let mut rng = rand::thread_rng();

    loop {
        if shutdown.load(Ordering::Relaxed) {
            // Send close frame
            frame_buf.clear();
            ws::encode_close_frame(1000, rng.r#gen(), &mut frame_buf);
            let _ = send_all(fd, &frame_buf);
            break;
        }

        let mut had_work = false;

        // 1. Pong responses (highest priority)
        while let Some(ping_data) = pong_rx.pop() {
            frame_buf.clear();
            ws::encode_pong_frame(&ping_data, rng.r#gen(), &mut frame_buf);
            send_all(fd, &frame_buf)?;
            had_work = true;
        }

        // 2. Outbound broadcast messages
        while let Some(json) = outbound.pop() {
            frame_buf.clear();
            ws::encode_text_frame(json.as_bytes(), rng.r#gen(), &mut frame_buf);
            send_all(fd, &frame_buf)?;
            had_work = true;
        }

        if !had_work {
            std::thread::sleep(std::time::Duration::from_micros(100));
        }
    }

    Ok(())
}

fn send_all(fd: RawFd, mut data: &[u8]) -> Result<(), std::io::Error> {
    while !data.is_empty() {
        let n = unsafe { libc::send(fd, data.as_ptr() as *const libc::c_void, data.len(), 0) };
        if n < 0 {
            return Err(std::io::Error::last_os_error());
        }
        data = &data[n as usize..];
    }
    Ok(())
}
