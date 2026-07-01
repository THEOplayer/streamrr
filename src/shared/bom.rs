use std::borrow::Cow;

const BOM: &str = "\u{feff}";

pub trait StripBom {
    /// Strip the byte-order mark (BOM) from a UTF-8 string.
    fn strip_bom(self) -> Self;
}

impl StripBom for &str {
    fn strip_bom(self) -> Self {
        self.strip_prefix(BOM).unwrap_or(self)
    }
}

impl StripBom for &[u8] {
    fn strip_bom(self) -> Self {
        self.strip_prefix(BOM.as_bytes()).unwrap_or(self)
    }
}

impl StripBom for &mut String {
    fn strip_bom(self) -> Self {
        if self.starts_with(BOM) {
            self.drain(0..BOM.len());
        }
        self
    }
}

impl StripBom for String {
    fn strip_bom(mut self) -> Self {
        (&mut self).strip_bom();
        self
    }
}

impl StripBom for &mut Vec<u8> {
    fn strip_bom(self) -> Self {
        if self.starts_with(BOM.as_bytes()) {
            self.drain(0..BOM.len());
        }
        self
    }
}

impl StripBom for Vec<u8> {
    fn strip_bom(mut self) -> Self {
        (&mut self).strip_bom();
        self
    }
}

impl<'a> StripBom for Cow<'a, str> {
    fn strip_bom(self) -> Self {
        match self {
            Cow::Borrowed(s) => Cow::Borrowed(s.strip_bom()),
            Cow::Owned(s) => Cow::Owned(s.strip_bom()),
        }
    }
}
