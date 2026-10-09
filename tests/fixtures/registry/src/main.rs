fn main() {
    println!("registry ok: {}", rostnix_fixture_dep::answer());
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_crate_of_the_registry_answers() {
        assert_eq!(rostnix_fixture_dep::answer(), 42);
    }
}
