fn main() -> anyhow::Result<()> {
    println!("{}", hello::greeting()?);
    Ok(())
}
