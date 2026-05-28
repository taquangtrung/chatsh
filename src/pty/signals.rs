use std::io;

use anyhow::Result;

const SIGWINCH: i32 = 28;

pub fn forward_sigwinch() -> Result<io::Result<()>> {
    Ok(Ok(()))
}

// ========================================================================
// Tests
// ========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sigwinch_constant() {
        assert_eq!(SIGWINCH, 28);
    }
}
