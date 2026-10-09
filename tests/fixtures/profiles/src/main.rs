#[derive(serde::Serialize)]
struct Report {
    value: u32,
}

fn assert_serialize<T: serde::Serialize>(_: &T) {}

fn main() {
    let report = Report { value: 12345 };
    assert_serialize(&report);
    let mut buf = itoa::Buffer::new();
    println!("profiles ok {}", buf.format(report.value));
}

#[cfg(test)]
mod tests {
    #[test]
    fn formats() {
        let mut buf = itoa::Buffer::new();
        assert_eq!(buf.format(12345u32), "12345");
    }

    // The release profile says panic = "abort". Cargo builds tests, and
    // everything under them, to unwind all the same.
    #[test]
    #[should_panic(expected = "caught")]
    fn a_panic_unwinds() {
        panic!("caught");
    }
}
