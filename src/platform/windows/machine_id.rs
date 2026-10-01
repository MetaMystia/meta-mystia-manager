//! 机器标识（`MachineGuid`）。

use super::registry;
use windows_sys::Win32::System::Registry::HKEY_LOCAL_MACHINE;

/// 读取注册表中的 `MachineGuid`。
pub fn machine_id() -> Option<String> {
    registry::read_string(
        HKEY_LOCAL_MACHINE,
        r"SOFTWARE\Microsoft\Cryptography",
        "MachineGuid",
    )
}
