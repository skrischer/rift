// Fixture for the rust-analyzer spike. The body holds a deliberate type error
// (`u32` initialized with a `&str`) so the server publishes at least one
// diagnostic for this file. Do not "fix" it — the error is the test subject.
pub fn answer() -> u32 {
    let value: u32 = "not a number";
    value
}
