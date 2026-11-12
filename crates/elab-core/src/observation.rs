//! Recorded values and comparisons shared by execution and offline verification.
use crate::{spec::Relation, ElabError, Result, Value};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", content = "value", rename_all = "snake_case")]
pub enum Observation {
    Scalar(Value),
    Bytes(String),
}

impl Observation {
    pub fn display(&self) -> String {
        match self {
            Self::Scalar(v) => v.literal(),
            Self::Bytes(h) => format!("hex:{h}"),
        }
    }
}

#[derive(Debug, Default)]
pub struct Observations(pub BTreeMap<String, Vec<Observation>>);
impl Observations {
    pub fn insert(&mut self, phase: &str, name: &str, value: Observation) -> usize {
        let values = self.0.entry(format!("{phase}.{name}")).or_default();
        values.push(value);
        values.len()
    }
    pub fn get(&self, reference: &str) -> Result<&Observation> {
        let (key, index) = match reference.rsplit_once('#') {
            Some((key, n)) => (
                key,
                Some(n.parse::<usize>().map_err(|_| {
                    ElabError::msg(format!("invalid observation index '{reference}'"))
                })?),
            ),
            None => (reference, None),
        };
        let values = self
            .0
            .get(key)
            .ok_or_else(|| ElabError::msg(format!("missing observation '{reference}'")))?;
        match index {
            Some(i) => i
                .checked_sub(1)
                .and_then(|i| values.get(i))
                .ok_or_else(|| ElabError::msg(format!("missing observation '{reference}'"))),
            None if values.len() == 1 => Ok(&values[0]),
            None => Err(ElabError::msg(format!(
                "ambiguous observation '{reference}'; use #1, #2, ..."
            ))),
        }
    }
}

pub fn compare(
    left: &Observation,
    right: &Observation,
    relation: Relation,
    tolerance: f64,
) -> Result<bool> {
    if !tolerance.is_finite() || tolerance < 0.0 {
        return Err(ElabError::msg("invalid comparison tolerance"));
    }
    let equal = match (left, right) {
        (Observation::Bytes(a), Observation::Bytes(b)) => {
            if tolerance != 0.0 || relation == Relation::Greater {
                return Err(ElabError::msg(
                    "bytes support exact equal/not_equal comparisons only",
                ));
            }
            // Validate hexadecimal, so malformed evidence cannot compare successfully.
            hex::decode(a).map_err(|e| ElabError::msg(e.to_string()))?
                == hex::decode(b).map_err(|e| ElabError::msg(e.to_string()))?
        }
        (Observation::Scalar(a), Observation::Scalar(b)) => {
            let integer = |v: &Value| match v {
                Value::Int(n) => Some(*n as i128),
                Value::UInt(n) => Some(*n as i128),
                _ => None,
            };
            if let (Some(a), Some(b)) = (integer(a), integer(b)) {
                if relation == Relation::Greater {
                    return Ok(a > b && (a - b) as f64 > tolerance);
                }
                if tolerance == 0.0 {
                    a == b
                } else {
                    (a - b).abs() as f64 <= tolerance
                }
            } else if let (Value::Text(a), Value::Text(b)) = (a, b) {
                if tolerance != 0.0 || relation == Relation::Greater {
                    return Err(ElabError::msg(
                        "text supports exact equal/not_equal comparisons only",
                    ));
                }
                a == b
            } else {
                let (a, b) = (a.as_f64()?, b.as_f64()?);
                if !a.is_finite() || !b.is_finite() {
                    return Err(ElabError::msg("cannot compare nonfinite observations"));
                }
                if relation == Relation::Greater {
                    return Ok(a > b && a - b > tolerance);
                }
                (a - b).abs() <= tolerance
            }
        }
        _ => {
            return Err(ElabError::msg(
                "cannot compare bytes with scalar observations",
            ))
        }
    };
    Ok(if relation == Relation::NotEqual {
        !equal
    } else {
        equal
    })
}
