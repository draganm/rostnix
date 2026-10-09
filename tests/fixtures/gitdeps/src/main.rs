use serde::de::value::{Error, MapDeserializer};
use serde::Deserialize;

#[derive(Deserialize, Debug, PartialEq)]
struct Point {
    x: u32,
    y: u32,
}

/// Builds a point through the derived `Deserialize`, from a map that serde
/// itself can read.
fn point(x: u32, y: u32) -> Point {
    let fields = [("x", x), ("y", y)];
    Point::deserialize(MapDeserializer::<_, Error>::new(fields.into_iter())).expect("a point")
}

fn main() {
    let point = point(3, 4);
    let mut buffer = itoa::Buffer::new();
    println!("gitdeps ok: sum {}", buffer.format(point.x + point.y));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derive_from_git_fills_every_field() {
        assert_eq!(point(1, 2), Point { x: 1, y: 2 });
    }
}
