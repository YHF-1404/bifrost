//! Small socket-option helpers shared by the bifrost-server and
//! bifrost-client data-plane TCP sockets. Lives in `bifrost-net`
//! because this is the crate that already pulls in `libc`.

use std::io;
use std::os::fd::AsRawFd;

/// Set `SO_SNDBUF` on `fd` to `bytes`. Returns `Err` when `setsockopt`
/// fails (the data plane logs and continues with the kernel default).
///
/// Why we cap it: Linux's auto-tuned default is several MB, which —
/// when bifrost is layered inside a nested TCP tunnel like xray-core
/// or any TLS-wrapped TCP transport — hides the underlying
/// congestion from the *inner* TCP that's riding on the bifrost TAP.
/// CUBIC then keeps growing cwnd through the tunnel until bufferbloat
/// collapses bulk throughput to ~0. A small bound forces writes to
/// block earlier, which propagates backpressure into the TAP queue
/// where the inner TCP can actually see drops and back off.
pub fn set_send_buffer_size<F: AsRawFd>(fd: &F, bytes: u32) -> io::Result<()> {
    let val: libc::c_int = bytes as libc::c_int;
    // SAFETY: setsockopt(fd, SOL_SOCKET, SO_SNDBUF, &c_int, sizeof(c_int))
    // is the canonical signature; the pointer is to a value we own and
    // the size matches its type.
    let rc = unsafe {
        libc::setsockopt(
            fd.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_SNDBUF,
            &val as *const libc::c_int as *const libc::c_void,
            std::mem::size_of::<libc::c_int>() as libc::socklen_t,
        )
    };
    if rc < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}
