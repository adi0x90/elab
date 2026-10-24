use crate::{ElabError, Result};
use std::fmt;

#[derive(Debug, Clone)]
pub enum Value {
    Float(f64),
    Int(i64),
    UInt(u64),
    Text(String),
}

impl Value {
    pub fn as_f64(&self) -> Result<f64> {
        match self {
            Value::Float(f) => Ok(*f),
            Value::Int(i) => Ok(*i as f64),
            Value::UInt(u) => Ok(*u as f64),
            Value::Text(s) => s
                .trim()
                .parse::<f64>()
                .map_err(|_| ElabError::msg(format!("not a number: {s}"))),
        }
    }

    pub fn as_u64(&self) -> Result<u64> {
        match self {
            Value::UInt(u) => Ok(*u),
            Value::Int(i) if *i >= 0 => Ok(*i as u64),
            Value::Float(f) if *f >= 0.0 => Ok(*f as u64),
            Value::Text(s) => {
                let t = s.trim();
                if let Some(hex) = t.strip_prefix("0x") {
                    u64::from_str_radix(hex, 16)
                        .map_err(|_| ElabError::msg(format!("not an address: {s}")))
                } else {
                    t.parse::<u64>()
                        .map_err(|_| ElabError::msg(format!("not an address: {s}")))
                }
            }
            _ => Err(ElabError::msg(format!("not a positive integer: {self}"))),
        }
    }

    /// A literal GDB can parse, used when substituting ${var} into an expression.
    pub fn literal(&self) -> String {
        match self {
            Value::Float(f) => format!("{f:?}"),
            Value::Int(i) => i.to_string(),
            Value::UInt(u) => u.to_string(),
            Value::Text(s) => s.clone(),
        }
    }

    /// Parse a GDB `value="..."` payload.
    pub fn from_gdb(raw: &str) -> Value {
        let t = raw.trim();
        if let Some(hex) = t.strip_prefix("0x") {
            // GDB may append a symbol annotation: "0x42009170 <sensor_task+18>".
            let hex = hex.split_whitespace().next().unwrap_or(hex);
            if let Ok(u) = u64::from_str_radix(hex, 16) {
                return Value::UInt(u);
            }
        }
        if let Ok(i) = t.parse::<i64>() {
            return Value::Int(i);
        }
        if let Ok(f) = t.parse::<f64>() {
            return Value::Float(f);
        }
        Value::Text(t.to_string())
    }
}

impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.literal())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn annotated_address_is_parsed() {
        match Value::from_gdb("0x42009170 <sensor_task+18>") {
            Value::UInt(u) => assert_eq!(u, 0x42009170),
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn literals_round_trip() {
        assert_eq!(Value::Float(1.0).literal(), "1.0");
        assert_eq!(Value::Int(-128).literal(), "-128");
        assert_eq!(Value::UInt(0x42009170).literal(), "1107333488");
    }
}
