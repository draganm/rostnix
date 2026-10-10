/// The platform this build of the crate is for.
pub fn side() -> &'static str {
    env!("ROSTNIX_TABLES_SIDE")
}
