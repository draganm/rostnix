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
