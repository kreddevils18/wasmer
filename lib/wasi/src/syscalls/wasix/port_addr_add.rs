use super::*;
use crate::syscalls::*;

/// ### `port_addr_add()`
/// Adds another static address to the local port
///
/// ## Parameters
///
/// * `addr` - Address to be added
pub fn port_addr_add<M: MemorySize>(
    ctx: FunctionEnvMut<'_, WasiEnv>,
    ip: WasmPtr<__wasi_cidr_t, M>,
) -> Errno {
    debug!(
        "wasi[{}:{}]::port_addr_add",
        ctx.data().pid(),
        ctx.data().tid()
    );
    let env = ctx.data();
    let memory = env.memory_view(&ctx);
    let cidr = wasi_try!(crate::state::read_cidr(&memory, ip));
    wasi_try!(env
        .net()
        .ip_add(cidr.ip, cidr.prefix)
        .map_err(net_error_into_wasi_err));
    Errno::Success
}
