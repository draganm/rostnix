extern "C" {
    fn rostnix_twice(n: i32) -> i32;
}

/// a + 2b, of which C does the doubling.
#[no_mangle]
pub extern "C" fn rostnix_sum(a: i32, b: i32) -> i32 {
    a + unsafe { rostnix_twice(b) }
}

/// The platforms rostnix-tables was built for: as the build script was
/// told and saw, and as linked into this library.
pub fn sides() -> String {
    format!(
        "{} linked:{}",
        env!("ROSTNIX_MATH_SIDES"),
        rostnix_tables::side()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sums_through_c() {
        assert_eq!(rostnix_sum(1, 2), 5);
    }
}
