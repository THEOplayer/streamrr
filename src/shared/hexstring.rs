use std::fmt::Display;

/// A wrapper that prints a byte slice as a hexadecimal string.
pub struct HexString<T>(T);

pub fn hex<T: AsRef<[u8]>>(bytes: T) -> HexString<T> {
    HexString(bytes)
}

impl<T: AsRef<[u8]>> Display for HexString<T> {
    fn fmt(&self, f: &mut core::fmt::Formatter) -> core::fmt::Result {
        for byte in self.0.as_ref() {
            write!(f, "{byte:02x}")?;
        }
        Ok(())
    }
}
