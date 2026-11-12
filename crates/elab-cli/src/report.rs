use elab_core::{
    observation::{compare, Observation, Observations},
    spec::Relation,
    ElabError, Result,
};
use serde_json::Value;
use std::collections::BTreeMap;

pub fn events(text: &str) -> Result<Vec<Value>> {
    text.lines()
        .map(|line| Ok(serde_json::from_str(line)?))
        .collect()
}

/// Re-evaluate comparisons from earlier observations, not the recorded verdict.
pub fn verify_comparisons(events: &[Value]) -> Result<()> {
    let mut values = Observations::default();
    for e in events {
        match e["event"].as_str() {
            Some("observation") => {
                let phase = string(e, "phase")?;
                let name = string(e, "name")?;
                let n = values.insert(phase, name, serde_json::from_value(e["value"].clone())?);
                if e["occurrence"].as_u64() != Some(n as u64) {
                    return Err(ElabError::msg("observation occurrence mismatch"));
                }
            }
            Some("compare") => {
                let relation: Relation = serde_json::from_value(e["relation"].clone())?;
                let tolerance = e["tolerance"]
                    .as_f64()
                    .ok_or_else(|| ElabError::msg("missing comparison tolerance"))?;
                let ok = compare(
                    values.get(string(e, "left")?)?,
                    values.get(string(e, "right")?)?,
                    relation,
                    tolerance,
                )?;
                if !ok || e["ok"].as_bool() != Some(true) {
                    return Err(ElabError::msg("recorded comparison failed verification"));
                }
            }
            _ => (),
        }
    }
    Ok(())
}
fn string<'a>(e: &'a Value, key: &str) -> Result<&'a str> {
    e[key]
        .as_str()
        .ok_or_else(|| ElabError::msg(format!("missing event field '{key}'")))
}
fn escape(s: &str) -> String {
    s.replace('|', "\\|").replace(['\n', '\r'], " ")
}

pub fn render(events: &[Value], markdown: bool) -> Result<String> {
    let mut phases: Vec<String> = Vec::new();
    let mut names: Vec<(String, usize)> = Vec::new();
    let mut cells = BTreeMap::new();
    let mut counts: BTreeMap<(String, String), usize> = BTreeMap::new();
    for e in events {
        if e["event"] == "phase" {
            let phase = string(e, "name")?.to_string();
            if !phases.contains(&phase) {
                phases.push(phase);
            }
        }
        if e["event"] != "observation" {
            continue;
        }
        let phase = string(e, "phase")?.to_string();
        if !phases.contains(&phase) {
            phases.push(phase.clone());
        }
        let name = string(e, "name")?.to_string();
        let index = counts.entry((phase.clone(), name.clone())).or_default();
        *index += 1;
        let value: Observation = serde_json::from_value(e["value"].clone())?;
        let key = (name.clone(), *index);
        if !names.contains(&key) {
            names.push(key);
        }
        cells.insert((phase, name, *index), value);
    }
    if names.is_empty() {
        return Ok("No comparable observations recorded in this run.\n".into());
    }
    let mut rows = vec![std::iter::once("Observation".to_string())
        .chain(phases.iter().cloned())
        .collect::<Vec<_>>()];
    for (name, index) in names {
        let repeated = counts
            .iter()
            .any(|((_, n), count)| n == &name && *count > 1);
        let label = if repeated {
            format!("{name}#{index}")
        } else {
            name.clone()
        };
        let mut row = vec![label];
        let baseline = cells.get(&(phases[0].clone(), name.clone(), index));
        for (p, phase) in phases.iter().enumerate() {
            let cell = match cells.get(&(phase.clone(), name.clone(), index)) {
                None => "missing".to_string(),
                Some(value) => {
                    let mut text = value.display();
                    if text.chars().count() > 100 {
                        text = format!("{}…", text.chars().take(96).collect::<String>());
                    }
                    if p > 0 {
                        if let (Observation::Scalar(current), Some(Observation::Scalar(base))) =
                            (value, baseline)
                        {
                            if let (Ok(c), Ok(b)) = (current.as_f64(), base.as_f64()) {
                                if c.is_finite() && b.is_finite() {
                                    text.push_str(&format!(" (delta {:+.6})", c - b));
                                }
                            }
                        }
                    }
                    text
                }
            };
            row.push(cell);
        }
        rows.push(row);
    }
    let mut out = String::new();
    if markdown {
        for (i, row) in rows.iter().enumerate() {
            out.push_str(&format!(
                "| {} |\n",
                row.iter()
                    .map(|s| escape(s))
                    .collect::<Vec<_>>()
                    .join(" | ")
            ));
            if i == 0 {
                out.push_str(&format!("| {} |\n", vec!["---"; row.len()].join(" | ")));
            }
        }
    } else {
        let widths: Vec<usize> = (0..rows[0].len())
            .map(|i| rows.iter().map(|r| r[i].chars().count()).max().unwrap_or(0))
            .collect();
        for row in rows {
            for (i, cell) in row.iter().enumerate() {
                out.push_str(cell);
                out.push_str(&" ".repeat(widths[i] - cell.chars().count() + 2));
            }
            out.push('\n');
        }
    }
    out.push_str(&format!(
        "\nNumeric deltas are relative to {}. Each cell retains its own observation timing.\n",
        escape(&phases[0])
    ));
    for e in events.iter().filter(|e| e["event"] == "compare") {
        out.push_str(&format!(
            "\n{}: {} {:?} {} (tolerance {}){}\n",
            if e["ok"] == true { "PASS" } else { "FAIL" },
            escape(string(e, "left")?),
            serde_json::from_value::<Relation>(e["relation"].clone())?,
            escape(string(e, "right")?),
            e["tolerance"],
            e["error"]
                .as_str()
                .map(|s| format!(": {}", escape(s)))
                .unwrap_or_default()
        ));
    }
    Ok(out)
}
