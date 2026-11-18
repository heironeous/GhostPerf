//! Non-Windows stand-in: there's no portable way to raise another
//! process's window, so an already-running UI is just left as is.

pub fn bring_to_front(_pid: u32) -> bool {
    false
}
