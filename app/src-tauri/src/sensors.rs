//! What the system says about the network and the power, for the network rules (see
//! `shardload_manager::net`). Anything it cannot read counts as unmetered, on mains, unnamed.

use shardload_manager::net::Conditions;

/// The conditions now. Blocking: the manager calls it on a blocking thread.
pub fn read() -> Conditions {
    let (metered, network) = network();
    Conditions {
        metered,
        on_battery: on_battery(),
        network,
    }
}

/// Whether the connection to the internet costs by the byte, and its name. Windows says
/// both for the connection it uses for the internet; the name of a Wi-Fi network may be held
/// back without location access, and then there is none.
#[cfg(windows)]
fn network() -> (bool, Option<String>) {
    use ::windows::Networking::Connectivity::{NetworkCostType, NetworkInformation};
    let Ok(profile) = NetworkInformation::GetInternetConnectionProfile() else {
        return (false, None);
    };
    let metered = profile.GetConnectionCost().is_ok_and(|cost| {
        let kind = cost.NetworkCostType().unwrap_or(NetworkCostType::Unknown);
        matches!(kind, NetworkCostType::Fixed | NetworkCostType::Variable)
            || cost.Roaming().unwrap_or(false)
            || cost.OverDataLimit().unwrap_or(false)
    });
    let name = profile
        .ProfileName()
        .ok()
        .map(|n| n.to_string())
        .filter(|n| !n.trim().is_empty());
    (metered, name)
}

/// NetworkManager's word on the connection (over D-Bus, with `busctl`), when it runs.
#[cfg(not(windows))]
fn network() -> (bool, Option<String>) {
    const NM: [&str; 2] = [
        "org.freedesktop.NetworkManager",
        "/org/freedesktop/NetworkManager",
    ];
    let property = |path: &str, interface: &str, name: &str| -> Option<String> {
        let out = std::process::Command::new("busctl")
            .args(["--system", "get-property", NM[0], path, interface, name])
            .output()
            .ok()
            .filter(|o| o.status.success())?;
        Some(String::from_utf8_lossy(&out.stdout).trim().to_owned())
    };
    // `u 1` yes, `u 3` guessed yes; 2 and 4 are no, 0 unknown.
    let metered =
        property(NM[1], NM[0], "Metered").is_some_and(|v| matches!(v.as_str(), "u 1" | "u 3"));
    let name = property(NM[1], NM[0], "PrimaryConnection")
        .and_then(|v| v.strip_prefix("o ").map(|p| p.trim_matches('"').to_owned()))
        .filter(|p| p != "/")
        .and_then(|p| property(&p, "org.freedesktop.NetworkManager.Connection.Active", "Id"))
        .and_then(|v| v.strip_prefix("s ").map(|n| n.trim_matches('"').to_owned()))
        .filter(|n| !n.is_empty());
    (metered, name)
}

#[cfg(windows)]
#[allow(unsafe_code)]
fn on_battery() -> bool {
    use windows_sys::Win32::System::Power::{GetSystemPowerStatus, SYSTEM_POWER_STATUS};
    // SAFETY: a plain struct filled in by the call.
    let mut status: SYSTEM_POWER_STATUS = unsafe { std::mem::zeroed() };
    // SAFETY: `status` is valid for the call to write.
    let ok = unsafe { GetSystemPowerStatus(&mut status) } != 0;
    // AC line 0: off; battery flag 128: no battery at all.
    ok && status.ACLineStatus == 0 && status.BatteryFlag != 128
}

/// On battery: a battery discharging and no mains supply online.
#[cfg(not(windows))]
fn on_battery() -> bool {
    let Ok(dir) = std::fs::read_dir("/sys/class/power_supply") else {
        return false;
    };
    let read = |p: &std::path::Path, f: &str| {
        std::fs::read_to_string(p.join(f))
            .map(|s| s.trim().to_owned())
            .unwrap_or_default()
    };
    let (mut mains, mut discharging) = (false, false);
    for entry in dir.flatten() {
        let p = entry.path();
        match read(&p, "type").as_str() {
            "Mains" if read(&p, "online") == "1" => mains = true,
            "Battery" if read(&p, "status") == "Discharging" => discharging = true,
            _ => {}
        }
    }
    discharging && !mains
}

#[cfg(test)]
mod tests {
    #[test]
    fn reads_without_failing() {
        // Whatever this machine is: it must not panic or hang.
        let c = super::read();
        println!("{c:?}");
    }
}
