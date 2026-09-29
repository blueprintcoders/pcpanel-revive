//! Turning single monitors off and on over their cable (DDC/CI power mode, VCP 0xD6), shared by
//! Windows and Linux.

/// The DDC/CI power code a monitor accepts for "off": 5 if it lists it, else 4 (standby), else 5.
pub fn off_code(caps: &str) -> u32 {
    let lower = caps.to_lowercase();
    let codes: Vec<u32> = lower.find("d6(")
        .and_then(|at| lower[at + 3..].split(')').next())
        .map(|list| list.split_whitespace().filter_map(|c| u32::from_str_radix(c, 16).ok()).collect())
        .unwrap_or_default();
    if codes.contains(&5) || !codes.contains(&4) { 5 } else { 4 }
}

#[cfg(test)]
mod tests {
    #[test]
    fn picks_the_off_code_the_monitor_supports() {
        // Real capability strings from an LG MP67 and WK95U: only 1 (on) and 4 (standby).
        assert_eq!(super::off_code("vcp(02 04 C9D6(01 04)DFE0E1E3(00 01))"), 4);
        assert_eq!(super::off_code("vcp(02 04 D6(01 04) DF 62)"), 4);
        assert_eq!(super::off_code("vcp(D6(01 04 05))"), 5);
        assert_eq!(super::off_code("no d6 listed"), 5);
    }
}
