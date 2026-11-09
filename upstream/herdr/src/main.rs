// Modified for Viva #191: the standalone executable and Viva now share the
// same imported runtime implementation in lib.rs (Apache-2.0 retained).
fn main() -> std::io::Result<()> {
    herdr::run()
}
