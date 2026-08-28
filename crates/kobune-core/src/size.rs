//! Sizes as a person reads them.
//!
//! **Here because both sides of the socket show byte counts.** The daemon
//! puts them in the progress it sends with an event; the CLI puts them in
//! the bar it draws. That had been a copy each, and the copies drifted:
//! same rounding, same units, different argument types, and a `GB` arm
//! that had to be remembered twice.
//!
//! `kobune-api` is the other crate both can see, and it takes no
//! human-facing formatting by rule (`docs/DESIGN.md` §3, §13). This one is
//! below both and side-effect-free, which is what is left.

/// A byte count, short enough to sit on a progress line.
///
/// 1024 to the unit and labelled kB/MB/GB, which is what `docker build`
/// prints — and these numbers are read next to its.
pub fn bytes(count: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = 1024 * KB;
    const GB: u64 = 1024 * MB;

    // One decimal place: the digit after it changes faster than a screen
    // is repainted, and reads as noise.
    //
    // A build context is the one thing here that reaches gigabytes, and
    // `3420.5 MB` is not a number anybody reads as "this is far too much".
    if count >= GB {
        format!("{}.{} GB", count / GB, (count % GB) * 10 / GB)
    } else if count >= MB {
        format!("{}.{} MB", count / MB, (count % MB) * 10 / MB)
    } else {
        format!("{} kB", count / KB)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_read_as_sizes() {
        assert_eq!(bytes(0), "0 kB");
        assert_eq!(bytes(4096), "4 kB");
        assert_eq!(bytes(1024 * 1024), "1.0 MB");
        assert_eq!(bytes(7_654_321), "7.2 MB");
        // What the build context reaches, and what made this arm worth
        // having: `3420.5 MB` does not read as a mistake, `3.1 GB` does.
        assert_eq!(bytes(3_342_664_218), "3.1 GB");
    }

    /// The boundaries, which is where a unit that is chosen by comparison
    /// gets it wrong.
    #[test]
    fn a_unit_changes_where_it_should() {
        assert_eq!(bytes(1024 * 1024 - 1), "1023 kB");
        assert_eq!(bytes(1024 * 1024 * 1024 - 1), "1023.9 MB");
        assert_eq!(bytes(1024 * 1024 * 1024), "1.0 GB");
        assert_eq!(bytes(u64::MAX), "17179869183.9 GB");
    }
}
