fn main() {
    let mut buf = itoa::Buffer::new();
    println!("ws-tool ok {}", buf.format(7));
}
