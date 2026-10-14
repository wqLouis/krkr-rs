//! Host process memory usage.
//!
//! The debug overlay shows a resident-set figure, but only the platform layer
//! may read a host path. [`memory_usage`] is a stateless free function: it
//! reads `/proc/self/status` on every call and holds no cache, so the caller's
//! refresh cadence is the only state.

/// Resident set size (RSS) of this process in bytes, or `None` when the host
/// does not expose one.
///
/// Linux and Android report it in `/proc/self/status` as `VmRSS`, in kibibytes;
/// the value is converted to bytes with a checked multiply. Any other target
/// has no reader and returns `None`. A missing or unreadable file also returns
/// `None`: the caller treats that as "unknown" and omits the figure rather than
/// substituting a value.
pub fn memory_usage() -> Option<u64> {
    if !(cfg!(target_os = "linux") || cfg!(target_os = "android")) {
        return None;
    }
    let text = match std::fs::read_to_string("/proc/self/status") {
        Ok(text) => text,
        Err(error) => {
            log::debug!("cannot read /proc/self/status: {error}");
            return None;
        }
    };
    parse_vm_rss(&text)
}

/// Parses the `VmRSS` line of `/proc/self/status` into bytes.
///
/// The line is `VmRSS:\t<kib> kB`; a file without the line, a value that does
/// not parse, or a byte count that overflows `u64` all yield `None`.
fn parse_vm_rss(text: &str) -> Option<u64> {
    for line in text.lines() {
        let Some(rest) = line.strip_prefix("VmRSS:") else {
            continue;
        };
        let value = rest.split_whitespace().next()?;
        let kibibytes: u64 = match value.parse() {
            Ok(kibibytes) => kibibytes,
            Err(error) => {
                log::warn!("cannot parse VmRSS value {value:?}: {error}");
                return None;
            }
        };
        return kibibytes.checked_mul(1024);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_vmrss_line_from_a_status_file() {
        let text = "Name:\tkrkr\nVmPeak:\t  123456 kB\nVmRSS:\t   1234 kB\nRssAnon:\t  1000 kB\n";
        assert_eq!(parse_vm_rss(text), Some(1234 * 1024));
    }

    #[test]
    fn missing_or_malformed_vmrss_is_unknown() {
        assert_eq!(parse_vm_rss("Name:\tkrkr\n"), None);
        assert_eq!(parse_vm_rss("VmRSS:\tnot-a-number kB\n"), None);
        assert_eq!(parse_vm_rss("VmRSS:\n"), None);
        // A value that parses but overflows the byte conversion is unknown,
        // not wrapped.
        assert_eq!(parse_vm_rss("VmRSS:\t18446744073709551615 kB\n"), None);
    }
}
