use windows::Win32::System::SystemInformation::GetTickCount;
use windows::Win32::UI::Input::KeyboardAndMouse::{GetLastInputInfo, LASTINPUTINFO};

/// Milliseconds since the last keyboard/mouse input in this session, or `None` if Windows will not
/// say.
///
/// Unknown idle time means do not record (CLAUDE.md, PRIVACY.md): the recorder treats `None` as
/// idle, exactly like the Plasma and macOS backends. Until 2026-10-16 a failure here reported
/// "active" instead (remediation F4), so the Windows recorder kept storing frames when the one
/// platform that can tell whether the user is present could not.
///
/// Both values are 32-bit tick counts that wrap every ~49.7 days; the wrapping subtraction is
/// correct across one wrap, and idle spans longer than 49.7 days are not meaningful anyway.
pub fn idle_millis() -> Option<u64> {
    let mut info = LASTINPUTINFO {
        cbSize: size_of::<LASTINPUTINFO>() as u32,
        dwTime: 0,
    };
    // SAFETY: `info` is a properly sized LASTINPUTINFO, valid for the call.
    if !unsafe { GetLastInputInfo(&mut info) }.as_bool() {
        tracing::debug!("GetLastInputInfo failed; idle time unknown");
        return None;
    }
    // SAFETY: no arguments. Must be the 32-bit GetTickCount: dwTime is on the same 32-bit clock.
    let now = unsafe { GetTickCount() };
    Some(idle_from_ticks(now, info.dwTime))
}

pub(crate) fn idle_from_ticks(now: u32, last_input: u32) -> u64 {
    u64::from(now.wrapping_sub(last_input))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_difference() {
        assert_eq!(idle_from_ticks(10_000, 4_000), 6_000);
        assert_eq!(idle_from_ticks(5, 5), 0);
    }

    #[test]
    fn survives_tick_wraparound() {
        // Input 100 ticks before the counter wraps to 0, now 50 ticks after it.
        assert_eq!(idle_from_ticks(50, u32::MAX - 99), 150);
        assert_eq!(idle_from_ticks(0, u32::MAX), 1);
    }

    #[test]
    fn live_value_is_sane() {
        // Can be large on an unattended machine, but never beyond one tick-count period. From a
        // session without input state (a service) it may be unknown, which is also valid.
        if let Some(ms) = idle_millis() {
            assert!(ms <= u64::from(u32::MAX));
        }
    }
}
