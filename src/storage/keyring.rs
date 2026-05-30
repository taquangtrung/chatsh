use anyhow::Result;

const SERVICE: &str = "com.chatsh";

pub fn init() {
    let _ = keyring::use_native_store(false);
}

pub fn set_secret(key: &str, value: &str) -> Result<()> {
    let entry = keyring_core::Entry::new(SERVICE, key)?;
    entry.set_password(value)?;
    Ok(())
}

pub fn get_secret(key: &str) -> Result<Option<String>> {
    let entry = match keyring_core::Entry::new(SERVICE, key) {
        Ok(e) => e,
        Err(_) => return Ok(None),
    };
    match entry.get_password() {
        Ok(v) => Ok(Some(v)),
        Err(keyring_core::Error::NoEntry) => Ok(None),
        Err(e) => Err(anyhow::anyhow!("keyring get: {e}")),
    }
}

// ========================================================================
// Tests
// ========================================================================

#[cfg(test)]
mod tests {
    use super::*;
}
